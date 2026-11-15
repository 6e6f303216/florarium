#!/usr/bin/env nix-shell
#![doc = ""] /*
#!nix-shell -p rustc ncurses zip -i bash
set -e
BIN="${TMPDIR:-/tmp}/florarium_ascii_bin"
rustc "$0" -O -o "$BIN"
exec "$BIN" "$@"
*/

//! ============================================================================
//! FLORARIUM — Полноэкранная консольная галерея ASCII-цветов в вазах и горшках
//! ============================================================================
//!
//! Особенности:
//! - 40 уникальных ботанических видов (включая кактусы и суккуленты) в ASCII-графике
//! - 10 видов ваз, кашпо, кувшинов и горшков (400 уникальных комбинаций)
//! - Полноэкранный режим терминала (Alternate Screen Buffer) с адаптивной вёрсткой
//! - Автоматическое скрытие подсказок управления через 5 секунд и режим без интерфейса (`U`)
//! - Нулевые внешние зависимости (только стандартная библиотека Rust `std`)
//! - Встроенная поддержка запуска через `nix-shell` и генерация `.zip` с `shell.nix`
//!   при запуске с флагом `--bundle`
//! ============================================================================

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Ширина холста для всех спрайтов цветов и ёмкостей (нечётная, центральная ось = 19)
const CANVAS_WIDTH: usize = 39;

/// RGB-цвет для TrueColor ANSI-последовательностей
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb(u8, u8, u8);

impl Rgb {
    /// ANSI-код цвета текста (foreground)
    fn fg(self) -> String {
        format!("\x1b[38;2;{};{};{}m", self.0, self.1, self.2)
    }

    /// ANSI-код цвета фона (background)
    fn bg(self) -> String {
        format!("\x1b[48;2;{};{};{}m", self.0, self.1, self.2)
    }

    /// Слегка осветлённый оттенок для бликов
    fn brighten(self, delta: i16) -> Rgb {
        let clamp = |v: u8| -> u8 { (v as i16 + delta).clamp(0, 255) as u8 };
        Rgb(clamp(self.0), clamp(self.1), clamp(self.2))
    }
}

/// Палитра раскраски цветка
#[derive(Clone, Copy, Debug)]
struct FlowerPalette {
    petal_main: Rgb,
    petal_shade: Rgb,
    center: Rgb,
    stem: Rgb,
    leaf: Rgb,
    accent: Rgb,
}

/// Описание одного ботанического вида (цветок + метаданные)
struct Flower {
    name_ru: &'static str,
    name_la: &'static str,
    family: &'static str,
    origin: &'static str,
    symbolism: &'static str,
    description: &'static str,
    palette: FlowerPalette,
    /// Индекс строки, начиная с которой начинается стебель и листья
    stem_start_row: usize,
    /// Строки ASCII-арта (центрированы относительно 19-го столбца)
    art: &'static [&'static str],
}

/// Палитра раскраски вазы или горшка
#[derive(Clone, Copy, Debug)]
struct VesselPalette {
    rim: Rgb,
    body_main: Rgb,
    body_shade: Rgb,
    pattern: Rgb,
}

/// Описание одной ёмкости (ваза, горшок, кашпо, чаша)
struct Vessel {
    name_ru: &'static str,
    category: &'static str,
    material: &'static str,
    description: &'static str,
    palette: VesselPalette,
    art: &'static [&'static str],
}

/// Цветовая тема оформления галереи
#[derive(Clone, Copy, Debug)]
struct GalleryTheme {
    name: &'static str,
    bg_outer: Rgb,
    bg_card: Rgb,
    border: Rgb,
    title: Rgb,
    text: Rgb,
    muted: Rgb,
    highlight: Rgb,
}

const THEMES: &[GalleryTheme] = &[
    GalleryTheme {
        name: "Полуночный Сад",
        bg_outer: Rgb(13, 17, 23),
        bg_card: Rgb(20, 26, 35),
        border: Rgb(62, 132, 118),
        title: Rgb(132, 235, 198),
        text: Rgb(226, 232, 240),
        muted: Rgb(134, 150, 168),
        highlight: Rgb(250, 204, 107),
    },
    GalleryTheme {
        name: "Ботанический Атлас",
        bg_outer: Rgb(18, 22, 18),
        bg_card: Rgb(26, 33, 26),
        border: Rgb(146, 132, 78),
        title: Rgb(238, 214, 134),
        text: Rgb(235, 234, 222),
        muted: Rgb(152, 158, 140),
        highlight: Rgb(148, 216, 136),
    },
    GalleryTheme {
        name: "Тёплый Пергамент",
        bg_outer: Rgb(26, 19, 16),
        bg_card: Rgb(36, 27, 23),
        border: Rgb(176, 112, 74),
        title: Rgb(246, 186, 132),
        text: Rgb(242, 228, 216),
        muted: Rgb(168, 144, 130),
        highlight: Rgb(240, 132, 108),
    },
    GalleryTheme {
        name: "Лунный Нефрит",
        bg_outer: Rgb(11, 22, 26),
        bg_card: Rgb(17, 32, 38),
        border: Rgb(78, 156, 174),
        title: Rgb(142, 232, 245),
        text: Rgb(224, 240, 244),
        muted: Rgb(126, 162, 174),
        highlight: Rgb(245, 178, 216),
    },
];

/// Быстрый криптостойкий по качеству распределения ГПСЧ (SplitMix64 + Xoshiro256**)
struct Rng {
    s: [u64; 4],
}

impl Rng {
    /// Инициализация зерном из системного времени, PID процесса и адреса в стеке
    fn from_entropy() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(42));
        let mut seed = now.as_nanos() as u64 ^ ((std::process::id() as u64) << 32);
        let stack_probe = &seed as *const u64 as u64;
        seed ^= stack_probe.rotate_left(17);

        let mut next_sm64 = || -> u64 {
            seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = seed;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };

        Self {
            s: [next_sm64(), next_sm64(), next_sm64(), next_sm64()],
        }
    }

    fn next_u64(&mut self) -> u64 {
        let result = (self.s[1].wrapping_mul(5)).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    fn gen_range(&mut self, upper: usize) -> usize {
        if upper <= 1 {
            return 0;
        }
        (self.next_u64() as usize) % upper
    }
}

/// RAII-охранник состояния терминала: гарантирует выход из Alternate Screen
/// и восстановление настроек `stty` при любом завершении программы.
struct TerminalGuard {
    saved_stty: Option<String>,
    fullscreen: bool,
}

impl TerminalGuard {
    fn enter_fullscreen() -> io::Result<Self> {
        let saved_stty = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()
            .ok()
            .and_then(|out| {
                if out.status.success() {
                    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                } else {
                    None
                }
            });

        // Переводим терминал в неканонический режим без эха с таймаутом 0.1с
        let _ = Command::new("stty")
            .args(["-icanon", "-echo", "min", "0", "time", "1"])
            .stdin(Stdio::inherit())
            .status();

        let mut stdout = io::stdout();
        // Переключаемся в альтернативный экран, скрываем курсор и очищаем буфер
        write!(stdout, "\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H")?;
        stdout.flush()?;

        Ok(Self {
            saved_stty,
            fullscreen: true,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.fullscreen {
            let mut stdout = io::stdout();
            let _ = write!(stdout, "\x1b[0m\x1b[?25h\x1b[?1049l");
            let _ = stdout.flush();
        }
        if let Some(ref state) = self.saved_stty {
            let _ = Command::new("stty")
                .arg(state)
                .stdin(Stdio::inherit())
                .status();
        } else {
            let _ = Command::new("stty")
                .arg("sane")
                .stdin(Stdio::inherit())
                .status();
        }
    }
}

/// Определяет текущие размеры терминала `(rows, cols)`
fn get_terminal_size() -> (usize, usize) {
    if let Ok(output) = Command::new("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut parts = text.split_whitespace();
            if let (Some(r), Some(c)) = (parts.next(), parts.next()) {
                if let (Ok(rows), Ok(cols)) = (r.parse::<usize>(), c.parse::<usize>()) {
                    if rows >= 10 && cols >= 20 {
                        return (rows, cols);
                    }
                }
            }
        }
    }

    let cols = env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100);
    let rows = env::var("LINES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(32);
    (rows, cols)
}

/// Считает видимую ширину строки в символах, игнорируя ANSI-последовательности
fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            width += 1;
        }
    }
    width
}

/// Дополняет строку пробелами справа до целевой видимой ширины
fn pad_right(s: &str, width: usize) -> String {
    let w = visible_width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - w))
    }
}

/// Центрирует строку внутри заданной видимой ширины
fn pad_center(s: &str, width: usize) -> String {
    let w = visible_width(s);
    if w >= width {
        s.to_string()
    } else {
        let left = (width - w) / 2;
        let right = width - w - left;
        format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
    }
}

/// Переносит текст по словам с учётом символов Unicode (кириллицы)
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if current_len == 0 {
            current.push_str(word);
            current_len = word_len;
        } else if current_len + 1 + word_len <= max_width {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + word_len;
        } else {
            lines.push(current);
            current = word.to_string();
            current_len = word_len;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Нормализует строку ASCII-спрайта до ровно `CANVAS_WIDTH` (39) символов
fn normalize_canvas_line(raw: &str) -> Vec<char> {
    let mut chars: Vec<char> = raw.chars().collect();
    if chars.len() > CANVAS_WIDTH {
        chars.truncate(CANVAS_WIDTH);
    }
    while chars.len() < CANVAS_WIDTH {
        chars.push(' ');
    }
    chars
}

/// Отрисовывает строку цветка с TrueColor-раскраской и опциональной анимацией пыльцы
fn render_flower_line(
    flower: &Flower,
    row_idx: usize,
    bg: Rgb,
    animate: bool,
    tick: usize,
    seed_salt: usize,
) -> String {
    let chars = normalize_canvas_line(flower.art[row_idx]);
    let is_blossom = row_idx < flower.stem_start_row;
    let mut out = String::with_capacity(256);
    out.push_str(&bg.bg());

    for (col_idx, &ch) in chars.iter().enumerate() {
        if ch == ' ' {
            // В пустых клетках вокруг верхней части цветка добавляем редкие мерцающие пылинки
            if animate && row_idx < flower.stem_start_row + 2 && col_idx > 2 && col_idx < CANVAS_WIDTH - 3 {
                let hash = (row_idx * 31 + col_idx * 17 + seed_salt * 13) % 97;
                if hash == 0 {
                    let phase = (tick + row_idx + col_idx) % 8;
                    let particle = match phase {
                        0 | 1 => Some(('.', flower.palette.accent)),
                        2 | 3 => Some(('*', flower.palette.center)),
                        4 => Some(('+', flower.palette.petal_main.brighten(35))),
                        _ => None,
                    };
                    if let Some((pch, pcol)) = particle {
                        out.push_str(&pcol.fg());
                        out.push(pch);
                        continue;
                    }
                }
            }
            out.push(' ');
            continue;
        }

        let (color, bold) = if is_blossom {
            match ch {
                '@' | 'O' | '0' | '#' | '%' | '&' => (flower.palette.center, true),
                'o' | '*' => {
                    // Внутренние звёздочки/кружки в центре бутона или блики лепестков
                    let dist_from_center = (col_idx as isize - 19).abs();
                    if dist_from_center <= 4 {
                        (flower.palette.center, true)
                    } else {
                        (flower.palette.accent, true)
                    }
                }
                '(' | ')' | '{' | '}' | '[' | ']' | '<' | '>' | '/' | '\\' | '|' => {
                    (flower.palette.petal_shade, false)
                }
                _ => (flower.palette.petal_main, true),
            }
        } else {
            match ch {
                '|' | '!' | 'I' | ':' | ';' => (flower.palette.stem, true),
                '*' | '+' | 'x' | 'X' | '@' | 'o' => (flower.palette.accent, true),
                _ => (flower.palette.leaf, false),
            }
        };

        if bold {
            out.push_str("\x1b[1m");
        }
        out.push_str(&color.fg());
        out.push(ch);
        if bold {
            out.push_str("\x1b[22m");
        }
    }

    out
}

/// Отрисовывает строку вазы или горшка с TrueColor-раскраской
fn render_vessel_line(vessel: &Vessel, row_idx: usize, bg: Rgb) -> String {
    let chars = normalize_canvas_line(vessel.art[row_idx]);
    let mut out = String::with_capacity(256);
    out.push_str(&bg.bg());

    for (col_idx, &ch) in chars.iter().enumerate() {
        if ch == ' ' {
            out.push(' ');
            continue;
        }

        let (color, bold) = if row_idx == 0 {
            (vessel.palette.rim, true)
        } else {
            match ch {
                '*' | '+' | 'o' | 'O' | '@' | '#' | '~' | '^' | 'v' | 'x' | 'X' | '%' | '&' => {
                    (vessel.palette.pattern, true)
                }
                '/' | '\\' | '|' | '(' | ')' | '[' | ']' | '{' | '}' => {
                    if col_idx >= 19 {
                        (vessel.palette.body_shade, false)
                    } else {
                        (vessel.palette.body_main, true)
                    }
                }
                _ => {
                    if col_idx > 21 {
                        (vessel.palette.body_shade, false)
                    } else {
                        (vessel.palette.body_main, false)
                    }
                }
            }
        };

        if bold {
            out.push_str("\x1b[1m");
        }
        out.push_str(&color.fg());
        out.push(ch);
        if bold {
            out.push_str("\x1b[22m");
        }
    }

    out
}

/// ============================================================================
/// КОЛЛЕКЦИЯ ИЗ 36 ДЕТАЛИЗИРОВАННЫХ ASCII-ЦВЕТОВ
/// (Все спрайты выровнены по центральной вертикальной оси: столбец 19)
/// ============================================================================
const FLOWERS: &[Flower] = &[
    // 1. Алая Роза
    Flower {
        name_ru: "Алая Роза",
        name_la: "Rosa rubiginosa",
        family: "Розовые (Rosaceae)",
        origin: "Европа и Передняя Азия",
        symbolism: "Страстная любовь, благородство, восхищение",
        description: "Классический многослойный бутон с бархатными лепестками и острыми шипами на крепком стебле.",
        palette: FlowerPalette {
            petal_main: Rgb(238, 54, 84),
            petal_shade: Rgb(172, 24, 52),
            center: Rgb(255, 198, 72),
            stem: Rgb(62, 168, 86),
            leaf: Rgb(44, 136, 68),
            accent: Rgb(218, 84, 96),
        },
        stem_start_row: 8,
        art: &[
            "               _..---.._               ",
            "             .'  _..._  '.             ",
            "            /  .'  _  '.  \\            ",
            "           |  /  .'@'.  \\  |           ",
            "           |  |  | @ |  |  |           ",
            "            \\  \\  '.'  /  /            ",
            "             '. '.___.' .'             ",
            "               '-._|_.-'               ",
            "                  \\|/                  ",
            "               .---'---.               ",
            "              /  / | \\  \\              ",
            "             '--'  |  '--'             ",
            "                  >|                   ",
            "                   |<                  ",
            "                   |                   ",
        ],
    },
    // 2. Подсолнух
    Flower {
        name_ru: "Подсолнух Солнечный",
        name_la: "Helianthus annuus",
        family: "Астровые (Asteraceae)",
        origin: "Северная Америка",
        symbolism: "Солнечная энергия, верность, оптимизм",
        description: "Могучий цветок солнца с золотой короной лепестков и тёмной корзиной созревающих семян.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 214, 48),
            petal_shade: Rgb(232, 162, 24),
            center: Rgb(98, 58, 32),
            stem: Rgb(86, 176, 68),
            leaf: Rgb(64, 148, 52),
            accent: Rgb(255, 240, 140),
        },
        stem_start_row: 9,
        art: &[
            "               \\  |  /                 ",
            "             '. \\ | / .'               ",
            "           --  .-'''-.  --             ",
            "          ==  / #O#O# \\  ==            ",
            "          == | #O#@#O# | ==            ",
            "          ==  \\ #O#O# /  ==            ",
            "           --  '-...-'  --             ",
            "             .' / | \\ '.               ",
            "               /  |  \\                 ",
            "                  ||                   ",
            "              .---||---.               ",
            "             /  / || \\  \\              ",
            "             '--' || '--'              ",
            "                  ||                   ",
        ],
    },
    // 3. Сакура
    Flower {
        name_ru: "Сакура (Ветка вишни)",
        name_la: "Prunus serrulata",
        family: "Розовые (Rosaceae)",
        origin: "Япония и Восточная Азия",
        symbolism: "Мимолётная красота жизни, весеннее обновление",
        description: "Изящная цветущая ветвь японской вишни с нежно-розовыми соцветиями и опадающими лепестками.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 188, 212),
            petal_shade: Rgb(236, 134, 172),
            center: Rgb(224, 68, 114),
            stem: Rgb(138, 92, 68),
            leaf: Rgb(108, 168, 94),
            accent: Rgb(255, 224, 236),
        },
        stem_start_row: 9,
        art: &[
            "          _.-._         _.-._          ",
            "         (  *  )       (  *  )         ",
            "          '-.-'   _.-._ '-.-'          ",
            "             \\   (  *  )  /            ",
            "         *    \\   '-.-'  /    *        ",
            "               \\    |   /              ",
            "            _.-.\\   |  /               ",
            "           (  *  )--+--      *         ",
            "            '-.-'   |                  ",
            "                   /                   ",
            "                  /  .---.             ",
            "                 |  /  /  )            ",
            "                  \\ '--'               ",
            "                   |                   ",
        ],
    },
    // 4. Королевский Тюльпан
    Flower {
        name_ru: "Королевский Тюльпан",
        name_la: "Tulipa gesneriana",
        family: "Лилейные (Liliaceae)",
        origin: "Средняя Азия и Нидерланды",
        symbolism: "Совершенная любовь, весна, изобилие",
        description: "Строгий бокаловидный бутон с шелковистыми лепестками, обрамлённый длинными мечевидными листьями.",
        palette: FlowerPalette {
            petal_main: Rgb(248, 86, 118),
            petal_shade: Rgb(204, 42, 78),
            center: Rgb(255, 224, 96),
            stem: Rgb(72, 184, 98),
            leaf: Rgb(54, 154, 82),
            accent: Rgb(255, 168, 186),
        },
        stem_start_row: 7,
        art: &[
            "              |\\   _   /|              ",
            "              | \\ / \\ / |              ",
            "              |  V   V  |              ",
            "              |         |              ",
            "              |         |              ",
            "               \\       /               ",
            "                '.___.'                ",
            "                   |                   ",
            "               .   |   .               ",
            "              / \\  |  / \\              ",
            "             |   | | |   |             ",
            "              \\   \\|/   /              ",
            "               '.  |  .'               ",
            "                 \\ | /                 ",
        ],
    },
    // 5. Белая Лилия
    Flower {
        name_ru: "Королевская Лилия",
        name_la: "Lilium candidum",
        family: "Лилейные (Liliaceae)",
        origin: "Средиземноморье",
        symbolism: "Чистота помыслов, величие, гармония",
        description: "Изысканный шестилепестковый цветок с грациозно отогнутыми краями и золотыми тычинками.",
        palette: FlowerPalette {
            petal_main: Rgb(248, 250, 255),
            petal_shade: Rgb(198, 214, 232),
            center: Rgb(248, 196, 44),
            stem: Rgb(68, 174, 92),
            leaf: Rgb(48, 142, 72),
            accent: Rgb(164, 224, 176),
        },
        stem_start_row: 8,
        art: &[
            "                 .-^-.                 ",
            "            .---' / \\ '---.            ",
            "          .'   .-' * '-.   '.          ",
            "         /   .'  * @ *  '.   \\         ",
            "         '--'   /  |  \\   '--'         ",
            "              .'   |   '.              ",
            "             /   .' '.   \\             ",
            "            '--''     ''--'            ",
            "                   |                   ",
            "                .--|--.                ",
            "               /  / \\  \\               ",
            "              '--' | '--'              ",
            "                   |                   ",
        ],
    },
    // 6. Орхидея Фаленопсис
    Flower {
        name_ru: "Орхидея Фаленопсис",
        name_la: "Phalaenopsis amabilis",
        family: "Орхидные (Orchidaceae)",
        origin: "Тропическая Юго-Восточная Азия",
        symbolism: "Утончённость, роскошь, редкая красота",
        description: "Экзотическая орхидея-бабочка с бархатной губой и каскадом воздушных крылатых цветков.",
        palette: FlowerPalette {
            petal_main: Rgb(232, 136, 246),
            petal_shade: Rgb(184, 78, 208),
            center: Rgb(255, 192, 64),
            stem: Rgb(76, 168, 94),
            leaf: Rgb(46, 132, 66),
            accent: Rgb(255, 226, 255),
        },
        stem_start_row: 9,
        art: &[
            "               .---.---.               ",
            "             .'  \\ @ /  '.             ",
            "            (   --(@)--   )            ",
            "             '.  / * \\  .'             ",
            "         .---. '-'---'-'               ",
            "       .' \\ @ / '. |                   ",
            "      (  --(@)--  )+                   ",
            "       '. / * \\ .' |                   ",
            "         '-----'   |                   ",
            "                   |                   ",
            "              .----'----.              ",
            "             /  /     \\  \\             ",
            "            '--'       '--'            ",
            "                   |                   ",
        ],
    },
    // 7. Синий Василёк
    Flower {
        name_ru: "Синий Василёк",
        name_la: "Centaurea cyanus",
        family: "Астровые (Asteraceae)",
        origin: "Европа",
        symbolism: "Нежность, верность, простота и ясность",
        description: "Полевой лазурный цветок с резными воронковидными краевыми лепестками и тонким стеблем.",
        palette: FlowerPalette {
            petal_main: Rgb(74, 146, 255),
            petal_shade: Rgb(44, 96, 214),
            center: Rgb(42, 54, 138),
            stem: Rgb(84, 178, 102),
            leaf: Rgb(66, 152, 84),
            accent: Rgb(154, 204, 255),
        },
        stem_start_row: 7,
        art: &[
            "             \\v/  \\v/  \\v/             ",
            "           >--  .-'''-.  --<           ",
            "          >--  / # * # \\  --<          ",
            "           >-- \\ # @ # / --<           ",
            "             /  '-...-'  \\             ",
            "            ^  /   |   \\  ^            ",
            "              ^    |    ^              ",
            "                   |                   ",
            "                 \\ | /                 ",
            "                  \\|/                  ",
            "                 / | \\                 ",
            "                /  |  \\                ",
            "                   |                   ",
        ],
    },
    // 8. Ромашка Полевая
    Flower {
        name_ru: "Ромашка Полевая",
        name_la: "Matricaria chamomilla",
        family: "Астровые (Asteraceae)",
        origin: "Евразия",
        symbolism: "Искренность, юность, светлая надежда",
        description: "Трогательный луговой цветок с выпуклой золотой сердцевиной и белоснежными лучами.",
        palette: FlowerPalette {
            petal_main: Rgb(250, 252, 255),
            petal_shade: Rgb(206, 218, 230),
            center: Rgb(255, 204, 38),
            stem: Rgb(82, 186, 86),
            leaf: Rgb(62, 156, 68),
            accent: Rgb(255, 238, 130),
        },
        stem_start_row: 8,
        art: &[
            "                .-. .-.                ",
            "             .-'  \\ /  '-.             ",
            "            (   .-'O'-.   )            ",
            "           (---( O@@@O )---)           ",
            "            (   '-.O.-'   )            ",
            "             '-.  / \\  .-'             ",
            "                '-' '-'                ",
            "                   |                   ",
            "                \\  |  /                ",
            "              -- \\ | / --              ",
            "                --\\|/--                ",
            "                 / | \\                 ",
            "                   |                   ",
        ],
    },
    // 9. Лотос Священный
    Flower {
        name_ru: "Лотос Священный",
        name_la: "Nelumbo nucifera",
        family: "Лотосовые (Nelumbonaceae)",
        origin: "Индия и Восточная Азия",
        symbolism: "Духовное пробуждение, чистота, спокойствие",
        description: "Величественный водный цветок с симметричными ярусами розово-жемчужных лепестков.",
        palette: FlowerPalette {
            petal_main: Rgb(252, 168, 206),
            petal_shade: Rgb(224, 108, 164),
            center: Rgb(255, 218, 74),
            stem: Rgb(64, 168, 114),
            leaf: Rgb(44, 138, 92),
            accent: Rgb(255, 224, 240),
        },
        stem_start_row: 7,
        art: &[
            "                 .-^-.                 ",
            "              .-' /|\\ '-.              ",
            "           .-' / / @ \\ \\ '-.           ",
            "         .'  /  | *@* |  \\  '.         ",
            "        (   (   \\  |  /   )   )        ",
            "         '.  \\   '-.-'   /  .'         ",
            "           '-.\\_________/.-'           ",
            "                   |                   ",
            "            .------'------.            ",
            "           (   .-------.   )           ",
            "            '-'    |    '-'            ",
            "                   |                   ",
        ],
    },
    // 10. Нарцисс Поэтический
    Flower {
        name_ru: "Нарцисс Поэтический",
        name_la: "Narcissus poeticus",
        family: "Амариллисовые (Amaryllidaceae)",
        origin: "Южная Европа",
        symbolism: "Весеннее пробуждение, вдохновение, самопознание",
        description: "Белоснежная звезда из шести лепестков с гофрированной шафраново-алой коронкой в центре.",
        palette: FlowerPalette {
            petal_main: Rgb(250, 250, 244),
            petal_shade: Rgb(214, 216, 202),
            center: Rgb(255, 138, 36),
            stem: Rgb(76, 178, 92),
            leaf: Rgb(56, 148, 72),
            accent: Rgb(255, 224, 72),
        },
        stem_start_row: 8,
        art: &[
            "                 /\\ /\\                 ",
            "             .--'  V  '--.             ",
            "            /    .---.    \\            ",
            "           <    ( *@* )    >           ",
            "            \\    '---'    /            ",
            "             '--.  ^  .--'             ",
            "                 \\/ \\/                 ",
            "                   |                   ",
            "                /  |  \\                ",
            "               |   |   |               ",
            "               |   |   |               ",
            "                \\  |  /                ",
            "                   |                   ",
        ],
    },
    // 11. Лаванда Узколистная
    Flower {
        name_ru: "Лаванда Прованская",
        name_la: "Lavandula angustifolia",
        family: "Яснотковые (Lamiaceae)",
        origin: "Средиземноморье",
        symbolism: "Умиротворение, безмятежность, преданность",
        description: "Ароматные фиолетовые колоски, поднимающиеся над серебристо-зелёной куртиной узких листьев.",
        palette: FlowerPalette {
            petal_main: Rgb(168, 124, 248),
            petal_shade: Rgb(124, 78, 212),
            center: Rgb(218, 188, 255),
            stem: Rgb(112, 168, 132),
            leaf: Rgb(92, 148, 114),
            accent: Rgb(232, 212, 255),
        },
        stem_start_row: 8,
        art: &[
            "                   *                   ",
            "              *   (*)   *              ",
            "             (*) (***) (*)             ",
            "            (***) (*) (***)            ",
            "             (*) (***) (*)             ",
            "            (***)  |  (***)            ",
            "             (*)   |   (*)             ",
            "              \\    |    /              ",
            "               \\   |   /               ",
            "              \\ \\  |  / /              ",
            "               \\ \\ | / /               ",
            "                \\_\\|/_/                ",
            "                   |                   ",
        ],
    },
    // 12. Мак Восточный
    Flower {
        name_ru: "Мак Восточный",
        name_la: "Papaver orientale",
        family: "Маковые (Papaveraceae)",
        origin: "Кавказ и Малая Азия",
        symbolism: "Сны, воображение, яркая вспышка чувств",
        description: "Шелковистая алая чаша с бархатисто-чёрной коробочкой тычинок и поникающим бутоном рядом.",
        palette: FlowerPalette {
            petal_main: Rgb(246, 56, 44),
            petal_shade: Rgb(192, 28, 24),
            center: Rgb(44, 38, 52),
            stem: Rgb(86, 174, 88),
            leaf: Rgb(62, 142, 68),
            accent: Rgb(255, 134, 112),
        },
        stem_start_row: 7,
        art: &[
            "             .---.   .---.             ",
            "           .'     '.'     '.           ",
            "          /    .-'###'-.    \\          ",
            "         |    (  #@#@#  )    |         ",
            "          \\    '-.###.-'    /          ",
            "           '.     '.'     .'           ",
            "             '---' | '---'   _         ",
            "                   |       .' '.       ",
            "                   |      (  *  )      ",
            "                \\  |  /    '-.-'       ",
            "               --\\ | /--     |         ",
            "                  \\|/       /          ",
            "                   |-------'           ",
        ],
    },
    // 13. Пион Древовидный
    Flower {
        name_ru: "Пион Императорский",
        name_la: "Paeonia suffruticosa",
        family: "Пионовые (Paeoniaceae)",
        origin: "Китай",
        symbolism: "Процветание, честь, пышное торжество",
        description: "Пышное облако кружевных лепестков с богатым объёмом и резными изумрудными листьями.",
        palette: FlowerPalette {
            petal_main: Rgb(248, 128, 178),
            petal_shade: Rgb(208, 72, 132),
            center: Rgb(255, 214, 92),
            stem: Rgb(62, 162, 84),
            leaf: Rgb(44, 132, 66),
            accent: Rgb(255, 196, 222),
        },
        stem_start_row: 8,
        art: &[
            "             .-~~~-.-~~~-.             ",
            "           .'  .-~' '~-.  '.           ",
            "          /  .'  .-@-.  '.  \\          ",
            "         |  (  ( *@*@* )  )  |         ",
            "         |  (   '-@@@-'   )  |         ",
            "          \\  '.  '---'  .'  /          ",
            "           '.  '-.___.-'  .'           ",
            "             '-.._____..-'             ",
            "                  \\|/                  ",
            "              .---'|'---.              ",
            "             /  /  |  \\  \\             ",
            "            '--'   |   '--'            ",
            "                   |                   ",
        ],
    },
    // 14. Ирис Бородатый
    Flower {
        name_ru: "Ирис Бородатый",
        name_la: "Iris germanica",
        family: "Ирисовые (Iridaceae)",
        origin: "Южная и Центральная Европа",
        symbolism: "Мудрость, вдохновение, королевское достоинство",
        description: "Скульптурный цветок с тремя сомкнутыми верхними долями и бархатными нижними лепестками.",
        palette: FlowerPalette {
            petal_main: Rgb(136, 94, 246),
            petal_shade: Rgb(88, 52, 198),
            center: Rgb(255, 204, 56),
            stem: Rgb(76, 174, 102),
            leaf: Rgb(56, 144, 82),
            accent: Rgb(196, 172, 255),
        },
        stem_start_row: 8,
        art: &[
            "                .-'''-.                ",
            "               /  / \\  \\               ",
            "              |  ( @ )  |              ",
            "            .-'\\  \\*/  /'-.            ",
            "          .'    '--|--'    '.          ",
            "         /   .-'   |   '-.   \\         ",
            "         '--'     / \\     '--'         ",
            "                 '---'                 ",
            "                   |                   ",
            "               /   |   \\               ",
            "              |    |    |              ",
            "               \\   |   /               ",
            "                   |                   ",
        ],
    },
    // 15. Хризантема
    Flower {
        name_ru: "Хризантема Осенняя",
        name_la: "Chrysanthemum morifolium",
        family: "Астровые (Asteraceae)",
        origin: "Восточная Азия",
        symbolism: "Долголетие, ясность ума, благородная осень",
        description: "Лучистый шар из десятков тонких игольчатых лепестков, расходящихся от золотого центра.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 176, 58),
            petal_shade: Rgb(224, 114, 34),
            center: Rgb(255, 232, 118),
            stem: Rgb(68, 166, 78),
            leaf: Rgb(48, 136, 58),
            accent: Rgb(255, 212, 128),
        },
        stem_start_row: 9,
        art: &[
            "               \\ \\ | / /               ",
            "             \\  \\ \\|/ /  /             ",
            "           -- \\--(*)--/ --             ",
            "          ==--( *(@@@)* )--==          ",
            "           -- /--(*)--\\ --             ",
            "             /  / /|\\ \\  \\             ",
            "               / / | \\ \\               ",
            "                  \\|/                  ",
            "                   |                   ",
            "               .---'---.               ",
            "              /  / | \\  \\              ",
            "             '--'  |  '--'             ",
            "                   |                   ",
        ],
    },
    // 16. Колокольчик Лесной
    Flower {
        name_ru: "Колокольчик Лесной",
        name_la: "Campanula rotundifolia",
        family: "Колокольчиковые (Campanulaceae)",
        origin: "Умеренный пояс Евразии",
        symbolism: "Нежность, благодарность, лесная тишина",
        description: "Тонкий изгибающийся стебель с поникающими лазурно-сиреневыми чашечками-колокольчиками.",
        palette: FlowerPalette {
            petal_main: Rgb(126, 158, 255),
            petal_shade: Rgb(84, 112, 224),
            center: Rgb(246, 226, 112),
            stem: Rgb(78, 182, 96),
            leaf: Rgb(56, 152, 74),
            accent: Rgb(188, 208, 255),
        },
        stem_start_row: 9,
        art: &[
            "                  .---.                ",
            "                 /     \\               ",
            "                /   *   \\              ",
            "               '-._____.-'             ",
            "             .---.  |                  ",
            "            /  *  \\ |   .---.          ",
            "           '-.___.-'|  /  *  \\         ",
            "                    | '-.___.-'        ",
            "                    |  /               ",
            "                   |-'                 ",
            "                \\  |  /                ",
            "                 \\ | /                 ",
            "                   |                   ",
        ],
    },
    // 17. Ландыш Майский
    Flower {
        name_ru: "Ландыш Майский",
        name_la: "Convallaria majalis",
        family: "Спаржевые (Asparagaceae)",
        origin: "Леса Европы",
        symbolism: "Возвращение счастья, чистота, майское утро",
        description: "Дугообразная кисть крошечных жемчужных колокольчиков в объятиях двух широких листьев.",
        palette: FlowerPalette {
            petal_main: Rgb(246, 252, 250),
            petal_shade: Rgb(196, 222, 214),
            center: Rgb(244, 218, 96),
            stem: Rgb(72, 184, 96),
            leaf: Rgb(46, 148, 72),
            accent: Rgb(214, 248, 234),
        },
        stem_start_row: 8,
        art: &[
            "                 .-----.               ",
            "                /  (o)  \\              ",
            "               |   (o)   |             ",
            "              /    (o)    \\            ",
            "             |     (o)     |           ",
            "             |      |      |           ",
            "             |      |      |           ",
            "              \\     |     /            ",
            "               \\    |    /             ",
            "                \\   |   /              ",
            "                 \\  |  /               ",
            "                  \\ | /                ",
            "                   \\|/                 ",
        ],
    },
    // 18. Одуванчик (Пушистик)
    Flower {
        name_ru: "Одуванчик Воздушный",
        name_la: "Taraxacum officinale",
        family: "Астровые (Asteraceae)",
        origin: "Евразия",
        symbolism: "Лёгкость бытия, исполнение желаний, ветер перемен",
        description: "Невесомый серебристый шар пушистых семян-парашютиков, готовых улететь от лёгкого дуновения.",
        palette: FlowerPalette {
            petal_main: Rgb(238, 246, 255),
            petal_shade: Rgb(178, 198, 218),
            center: Rgb(186, 164, 118),
            stem: Rgb(92, 182, 86),
            leaf: Rgb(66, 152, 64),
            accent: Rgb(255, 255, 255),
        },
        stem_start_row: 8,
        art: &[
            "               . * | * .        *      ",
            "             *  \\  |  /  *     /|\\     ",
            "            * -- * @ * -- *            ",
            "             *  /  |  \\  *   *         ",
            "               ' * | * '    /|\\        ",
            "                   |                   ",
            "                   |                   ",
            "                   |                   ",
            "               \\   |   /               ",
            "              --\\  |  /--              ",
            "               --\\ | /--               ",
            "                  \\|/                  ",
            "                   |                   ",
        ],
    },
    // 19. Кактус Цветущий
    Flower {
        name_ru: "Кактус Эхинопсис",
        name_la: "Echinopsis oxygona",
        family: "Кактусовые (Cactaceae)",
        origin: "Южная Америка",
        symbolism: "Стойкость, внутренняя сила, редкое чудо",
        description: "Ребристый пустынный кактус с колючками, увенчанный ярким ночным цветком-звездой.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 92, 168),
            petal_shade: Rgb(212, 46, 124),
            center: Rgb(255, 224, 84),
            stem: Rgb(64, 164, 88),
            leaf: Rgb(48, 134, 70),
            accent: Rgb(236, 228, 178),
        },
        stem_start_row: 5,
        art: &[
            "               \\  \\|/  /               ",
            "             --  ( @ )  --             ",
            "               /  /|\\  \\               ",
            "              .---'-'---.              ",
            "             /  |  |  |  \\             ",
            "           +| + | +| +| + |+           ",
            "            |   |  |  |   |            ",
            "           +| + | +| +| + |+           ",
            "            |   |  |  |   |            ",
            "           +| + | +| +| + |+           ",
            "             \\  |  |  |  /             ",
            "              '---------'              ",
        ],
    },
    // 20. Гортензия
    Flower {
        name_ru: "Гортензия Лазурная",
        name_la: "Hydrangea macrophylla",
        family: "Гортензиевые (Hydrangeaceae)",
        origin: "Япония и Корея",
        symbolism: "Искренность, душевное тепло, грация",
        description: "Пышное шаровидное соцветие, сотканное из множества нежных четырёхлепестковых цветков.",
        palette: FlowerPalette {
            petal_main: Rgb(114, 186, 255),
            petal_shade: Rgb(78, 138, 226),
            center: Rgb(224, 242, 255),
            stem: Rgb(68, 164, 88),
            leaf: Rgb(46, 134, 68),
            accent: Rgb(178, 218, 255),
        },
        stem_start_row: 8,
        art: &[
            "               .+-. .+-+.              ",
            "            .+( + )+( + )+.            ",
            "           ( + )+( @ )+( + )           ",
            "          ( + )+( + )+( + )+(          ",
            "           ( + )+( @ )+( + )           ",
            "            '+( + )+( + )+'            ",
            "               '+-' '-+'               ",
            "                  \\|/                  ",
            "              .---'|'---.              ",
            "             /  /  |  \\  \\             ",
            "            '--'   |   '--'            ",
            "                   |                   ",
        ],
    },
    // 21. Гвоздика Садовая
    Flower {
        name_ru: "Гвоздика Королевская",
        name_la: "Dianthus caryophyllus",
        family: "Гвоздичные (Caryophyllaceae)",
        origin: "Средиземноморье",
        symbolism: "Очарование, преданность, отвага",
        description: "Кружевной цветок с зубчатыми лепестками и пряным ароматом на стройном коленчатом стебле.",
        palette: FlowerPalette {
            petal_main: Rgb(244, 78, 114),
            petal_shade: Rgb(196, 38, 76),
            center: Rgb(255, 174, 192),
            stem: Rgb(86, 178, 124),
            leaf: Rgb(68, 152, 104),
            accent: Rgb(255, 218, 228),
        },
        stem_start_row: 6,
        art: &[
            "             /\\/\\/\\/\\/\\/\\              ",
            "            (  WWWWWWWW  )             ",
            "             \\  VVVVVV  /              ",
            "              '.      .'               ",
            "                \\____/                 ",
            "                 |__|                  ",
            "                   |                   ",
            "               \\   |   /               ",
            "                \\  |  /                ",
            "                   |                   ",
            "               \\   |   /               ",
            "                \\  |  /                ",
            "                   |                   ",
        ],
    },
    // 22. Гербера
    Flower {
        name_ru: "Гербера Огненная",
        name_la: "Gerbera jamesonii",
        family: "Астровые (Asteraceae)",
        origin: "Южная Африка",
        symbolism: "Радость жизни, открытость, солнечный привет",
        description: "Крупный яркий цветок с безупречной геометрией двойного ряда кораллово-оранжевых лепестков.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 118, 72),
            petal_shade: Rgb(224, 68, 42),
            center: Rgb(66, 44, 48),
            stem: Rgb(76, 178, 84),
            leaf: Rgb(54, 148, 64),
            accent: Rgb(255, 206, 84),
        },
        stem_start_row: 8,
        art: &[
            "              \\  \\ | /  /              ",
            "            -- \\ .-^-. / --            ",
            "          ==--  / *#* \\  --==          ",
            "          ==-- | *#@#* | --==          ",
            "          ==--  \\ *#* /  --==          ",
            "            -- / '-v-' \\ --            ",
            "              /  / | \\  \\              ",
            "                   |                   ",
            "                   |                   ",
            "               .---'---.               ",
            "              /  / | \\  \\              ",
            "             '--'  |  '--'             ",
            "                   |                   ",
        ],
    },
    // 23. Анютины Глазки
    Flower {
        name_ru: "Анютины Глазки",
        name_la: "Viola tricolor",
        family: "Фиалковые (Violaceae)",
        origin: "Европа",
        symbolism: "Нежные воспоминания, верность, задумчивость",
        description: "Бархатистая трёхцветная фиалка с выразительными тёмными лучиками в золотом глазке.",
        palette: FlowerPalette {
            petal_main: Rgb(148, 86, 238),
            petal_shade: Rgb(86, 44, 178),
            center: Rgb(255, 212, 48),
            stem: Rgb(74, 176, 92),
            leaf: Rgb(54, 146, 72),
            accent: Rgb(255, 238, 120),
        },
        stem_start_row: 7,
        art: &[
            "             .---.   .---.             ",
            "           .'     '.'     '.           ",
            "          (    \\   |   /    )          ",
            "           '-.--\\--@--/--.-'           ",
            "            /    / | \\    \\            ",
            "           (       *       )           ",
            "            '.___________.'            ",
            "                   |                   ",
            "                .--|--.                ",
            "               (  / \\  )               ",
            "                '-' | '-'              ",
            "                   |                   ",
        ],
    },
    // 24. Камелия Японская
    Flower {
        name_ru: "Камелия Японская",
        name_la: "Camellia japonica",
        family: "Чайные (Theaceae)",
        origin: "Япония, Корея и Китай",
        symbolism: "Восхищение, совершенство, верность",
        description: "Симметричная зимняя роза с восковыми лепестками и глянцевой тёмно-зелёной листвой.",
        palette: FlowerPalette {
            petal_main: Rgb(246, 96, 128),
            petal_shade: Rgb(198, 48, 84),
            center: Rgb(255, 216, 78),
            stem: Rgb(52, 146, 74),
            leaf: Rgb(36, 118, 56),
            accent: Rgb(255, 178, 196),
        },
        stem_start_row: 8,
        art: &[
            "               .-------.               ",
            "             .'  .---.  '.             ",
            "            /  .'  *  '.  \\            ",
            "           |  (  * @ *  )  |           ",
            "            \\  '.  *  .'  /            ",
            "             '.  '---'  .'             ",
            "               '-------'               ",
            "                  \\|/                  ",
            "              .---'|'---.              ",
            "             <   / | \\   >             ",
            "              '---'|'---'              ",
            "                   |                   ",
        ],
    },
    // 25. Крокус Весенний
    Flower {
        name_ru: "Крокус Весенний",
        name_la: "Crocus vernus",
        family: "Ирисовые (Iridaceae)",
        origin: "Альпы и Балканы",
        symbolism: "Первые лучи весны, юность, пробуждение",
        description: "Нежный весенний бокал с ярко-шафрановым пестиком, пробивающийся сквозь узкие травинки.",
        palette: FlowerPalette {
            petal_main: Rgb(178, 118, 255),
            petal_shade: Rgb(134, 72, 224),
            center: Rgb(255, 164, 34),
            stem: Rgb(86, 188, 98),
            leaf: Rgb(64, 158, 78),
            accent: Rgb(224, 192, 255),
        },
        stem_start_row: 7,
        art: &[
            "                /\\   /\\                ",
            "               /  \\ /  \\               ",
            "              |   *@*   |              ",
            "              |    |    |              ",
            "               \\       /               ",
            "                '.___.'                ",
            "                 \\ | /                 ",
            "               |  \\|/  |               ",
            "               |   |   |               ",
            "                \\  |  /                ",
            "                 \\ | /                 ",
            "                  \\|/                  ",
        ],
    },
    // 26. Эдельвейс Альпийский
    Flower {
        name_ru: "Эдельвейс Альпийский",
        name_la: "Leontopodium nivale",
        family: "Астровые (Asteraceae)",
        origin: "Высокогорья Альп и Карпат",
        symbolism: "Мужество, верность, покорение вершин",
        description: "Бархатистая серебристо-белая звезда высокогорий с золотыми пуговками соцветий в центре.",
        palette: FlowerPalette {
            petal_main: Rgb(238, 244, 248),
            petal_shade: Rgb(184, 198, 208),
            center: Rgb(246, 206, 68),
            stem: Rgb(118, 168, 128),
            leaf: Rgb(98, 148, 108),
            accent: Rgb(255, 255, 255),
        },
        stem_start_row: 8,
        art: &[
            "                  .-.                  ",
            "             .---'   '---.             ",
            "            (   .-*@*-.   )            ",
            "             '-'( @#@ )'-'             ",
            "            .---'-*@*-'---.            ",
            "           (   .'     '.   )           ",
            "            '-'         '-'            ",
            "                   |                   ",
            "                \\  |  /                ",
            "                 \\ | /                 ",
            "                /  |  \\                ",
            "                   |                   ",
        ],
    },
    // 27. Фуксия Гибридная
    Flower {
        name_ru: "Фуксия Балерина",
        name_la: "Fuchsia hybrida",
        family: "Кипрейные (Onagraceae)",
        origin: "Центральная и Южная Америка",
        symbolism: "Изящество, лёгкость, артистизм",
        description: "Поникающие цветки-фонарики с отогнутыми чашелистиками и длинными изящными тычинками.",
        palette: FlowerPalette {
            petal_main: Rgb(248, 68, 148),
            petal_shade: Rgb(142, 68, 224),
            center: Rgb(255, 194, 226),
            stem: Rgb(72, 168, 88),
            leaf: Rgb(52, 138, 68),
            accent: Rgb(255, 226, 134),
        },
        stem_start_row: 9,
        art: &[
            "               .---'---.               ",
            "              /    |    \\              ",
            "             '   .-'-.   '             ",
            "                (  @  )                ",
            "               / '---' \\               ",
            "                 | | |                 ",
            "                 * | *                 ",
            "                   |                   ",
            "              .----'----.              ",
            "             /  /  |  \\  \\             ",
            "            '--'   |   '--'            ",
            "                   |                   ",
        ],
    },
    // 28. Астра Осенняя
    Flower {
        name_ru: "Астра Звёздная",
        name_la: "Symphyotrichum novi-belgii",
        family: "Астровые (Asteraceae)",
        origin: "Северная Америка",
        symbolism: "Утончённость, терпение, звёздный свет",
        description: "Пушистая осенняя звезда с множеством узких сиренево-розовых лучей и солнечным диском.",
        palette: FlowerPalette {
            petal_main: Rgb(218, 126, 248),
            petal_shade: Rgb(172, 78, 212),
            center: Rgb(255, 206, 54),
            stem: Rgb(72, 168, 84),
            leaf: Rgb(52, 138, 64),
            accent: Rgb(244, 192, 255),
        },
        stem_start_row: 8,
        art: &[
            "               \\ | | | /               ",
            "             -- \\\\ | // --             ",
            "           ==-- ( *@* ) --==           ",
            "           ==-- ( @#@ ) --==           ",
            "             -- // | \\\\ --             ",
            "               / | | | \\               ",
            "                  \\|/                  ",
            "                   |                   ",
            "                \\  |  /                ",
            "                 \\ | /                 ",
            "                /  |  \\                ",
            "                   |                   ",
        ],
    },
    // 29. Гладиолус
    Flower {
        name_ru: "Гладиолус (Шпажник)",
        name_la: "Gladiolus communis",
        family: "Ирисовые (Iridaceae)",
        origin: "Средиземноморье и Африка",
        symbolism: "Сила характера, верность, победа",
        description: "Высокий стройный колос с ярусами раскрывающихся воронковидных цветков вдоль стебля.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 98, 108),
            petal_shade: Rgb(214, 54, 72),
            center: Rgb(255, 224, 128),
            stem: Rgb(74, 178, 92),
            leaf: Rgb(54, 148, 72),
            accent: Rgb(255, 174, 182),
        },
        stem_start_row: 9,
        art: &[
            "                  .^.                  ",
            "                 ( @ )                 ",
            "               .---'---.               ",
            "              ( *  |  * )              ",
            "               '---'---'               ",
            "             .-----'-----.             ",
            "            ( *    |    * )            ",
            "             '-----'-----'             ",
            "                  \\|/                  ",
            "               /   |   \\               ",
            "              |    |    |              ",
            "               \\   |   /               ",
            "                   |                   ",
        ],
    },
    // 30. Клевер Луговой
    Flower {
        name_ru: "Клевер Луговой",
        name_la: "Trifolium pratense",
        family: "Бобовые (Fabaceae)",
        origin: "Европа и Западная Азия",
        symbolism: "Удача, домашний уют, плодородие",
        description: "Пушистая малиново-розовая головка лугового клевера в окружении знаменитых тройчатых листьев.",
        palette: FlowerPalette {
            petal_main: Rgb(236, 104, 168),
            petal_shade: Rgb(188, 62, 126),
            center: Rgb(255, 184, 218),
            stem: Rgb(76, 182, 88),
            leaf: Rgb(52, 152, 68),
            accent: Rgb(168, 232, 164),
        },
        stem_start_row: 6,
        art: &[
            "                .-----.                ",
            "              .' * * * '.              ",
            "             ( * * @ * * )             ",
            "              '. * * * .'              ",
            "                '-----'                ",
            "                  \\|/                  ",
            "                   |                   ",
            "            .--.   |   .--.            ",
            "           (  .-)--|--(-.  )           ",
            "            '-'    |    '-'            ",
            "                   |                   ",
        ],
    },
    // 31. Стрелиция (Райская птица)
    Flower {
        name_ru: "Стрелиция (Райская птица)",
        name_la: "Strelitzia reginae",
        family: "Стрелициевые (Strelitziaceae)",
        origin: "Южная Африка",
        symbolism: "Свобода, полёт фантазии, экзотическое великолепие",
        description: "Яркий тропический цветок, напоминающий голову экзотической птицы с огненно-синим хохолком.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 132, 32),
            petal_shade: Rgb(62, 118, 248),
            center: Rgb(255, 214, 54),
            stem: Rgb(64, 166, 94),
            leaf: Rgb(44, 134, 72),
            accent: Rgb(124, 184, 255),
        },
        stem_start_row: 7,
        art: &[
            "                 /\\  /\\                ",
            "                /  \\/  \\               ",
            "               /  *  *  \\              ",
            "          .---'----------'---.         ",
            "         <======(  @  )=======>        ",
            "          '------.____.------'         ",
            "                   ||                  ",
            "                   ||                  ",
            "               /   ||   \\              ",
            "              |    ||    |             ",
            "               \\   ||   /              ",
            "                   ||                  ",
        ],
    },
    // 32. Незабудка Лесная
    Flower {
        name_ru: "Незабудка Лесная",
        name_la: "Myosotis sylvatica",
        family: "Бурачниковые (Boraginaceae)",
        origin: "Европа",
        symbolism: "Верная память, искренняя привязанность",
        description: "Нежная веточка с россыпью миниатюрных небесно-голубых цветков с солнечными глазками.",
        palette: FlowerPalette {
            petal_main: Rgb(118, 198, 255),
            petal_shade: Rgb(76, 152, 232),
            center: Rgb(255, 218, 64),
            stem: Rgb(78, 182, 94),
            leaf: Rgb(56, 152, 74),
            accent: Rgb(198, 236, 255),
        },
        stem_start_row: 8,
        art: &[
            "                 .---.                 ",
            "                (  @  )                ",
            "           .---. '---' .---.           ",
            "          (  @  )  |  (  @  )          ",
            "           '---'   |   '---'           ",
            "             \\     |     /             ",
            "              \\    |    /              ",
            "               '---+---'               ",
            "                   |                   ",
            "                .--|--.                ",
            "               /  / \\  \\               ",
            "              '--'   '--'              ",
            "                   |                   ",
        ],
    },
    // 33. Гибискус Китайский
    Flower {
        name_ru: "Гибискус Тропический",
        name_la: "Hibiscus rosa-sinensis",
        family: "Мальвовые (Malvaceae)",
        origin: "Восточная Азия",
        symbolism: "Тропическая страсть, гостеприимство, красота",
        description: "Крупный бархатный цветок с пятью широкими лепестками и длинным пестичным столбиком.",
        palette: FlowerPalette {
            petal_main: Rgb(246, 58, 94),
            petal_shade: Rgb(188, 28, 62),
            center: Rgb(255, 214, 68),
            stem: Rgb(58, 158, 78),
            leaf: Rgb(42, 128, 60),
            accent: Rgb(255, 168, 186),
        },
        stem_start_row: 8,
        art: &[
            "              .---. .---.              ",
            "            .'     '     '.            ",
            "           (     * @ *     )           ",
            "            '.   / | \\   .'            ",
            "            .'  '  *  '  '.            ",
            "           (       *       )           ",
            "            '.___________.'            ",
            "                  \\|/                  ",
            "              .---'|'---.              ",
            "             /  /  |  \\  \\             ",
            "            '--'   |   '--'            ",
            "                   |                   ",
        ],
    },
    // 34. Чертополох Шотландский
    Flower {
        name_ru: "Чертополох Шотландский",
        name_la: "Onopordum acanthium",
        family: "Астровые (Asteraceae)",
        origin: "Европа и Шотландия",
        symbolism: "Непокорность, защита, рыцарская честь",
        description: "Благородный колючий цветок с пурпурной кистью соцветий и резной охранной листвой.",
        palette: FlowerPalette {
            petal_main: Rgb(198, 98, 238),
            petal_shade: Rgb(148, 56, 192),
            center: Rgb(236, 174, 255),
            stem: Rgb(88, 168, 96),
            leaf: Rgb(66, 138, 74),
            accent: Rgb(214, 232, 168),
        },
        stem_start_row: 7,
        art: &[
            "              \\ \\ | | / /              ",
            "               \\ \\| |/ /               ",
            "              .-'-***-'-.              ",
            "             <  # # # #  >             ",
            "              '. # # # .'              ",
            "                '-._.-'                ",
            "                   |                   ",
            "              <\\   |   />              ",
            "               <\\  |  />               ",
            "                <\\ | />                ",
            "                   |                   ",
        ],
    },
    // 35. Эхеверия (Каменная роза)
    Flower {
        name_ru: "Эхеверия (Каменная роза)",
        name_la: "Echeveria elegans",
        family: "Толстянковые (Crassulaceae)",
        origin: "Мексика",
        symbolism: "Вечная любовь, гармония формы, терпение",
        description: "Геометрически безупречная розетка мясистых сизых листьев с высоким цветоносом коралловых колокольчиков.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 132, 118),
            petal_shade: Rgb(224, 86, 78),
            center: Rgb(255, 218, 96),
            stem: Rgb(102, 186, 164),
            leaf: Rgb(76, 156, 136),
            accent: Rgb(178, 236, 218),
        },
        stem_start_row: 5,
        art: &[
            "                  .---.                ",
            "                 /  *  \\               ",
            "                '-.   .-'              ",
            "                   |                   ",
            "                   |                   ",
            "              .-.  |  .-.              ",
            "            .'   '.|.'   '.            ",
            "          .'  .-.     .-.  '.          ",
            "         (   (   '. .'   )   )         ",
            "          '.  '-._____.-'  .'          ",
            "            '-------------'            ",
        ],
    },
    // 36. Лунный Цветок
    Flower {
        name_ru: "Лунный Цветок",
        name_la: "Ipomoea alba",
        family: "Вьюнковые (Convolvulaceae)",
        origin: "Тропическая Америка",
        symbolism: "Тайна ночи, мечтательность, тихое сияние",
        description: "Серебристо-белый ночной граммофон, раскрывающийся при свете звёзд и источающий тонкий аромат.",
        palette: FlowerPalette {
            petal_main: Rgb(228, 242, 255),
            petal_shade: Rgb(156, 192, 236),
            center: Rgb(174, 248, 218),
            stem: Rgb(68, 166, 118),
            leaf: Rgb(48, 136, 94),
            accent: Rgb(255, 244, 168),
        },
        stem_start_row: 8,
        art: &[
            "              .---------.              ",
            "            .'   \\ | /   '.            ",
            "           /    --(@)--    \\           ",
            "          |      / | \\      |          ",
            "           \\       *       /           ",
            "            '.           .'            ",
            "              '-._____.-'              ",
            "                   |                   ",
            "               .---'---.               ",
            "              (  /   \\  )              ",
            "               '-'   '-'               ",
            "                   |                   ",
        ],
    },
    // 37. Кактус Опунция
    Flower {
        name_ru: "Кактус Опунция",
        name_la: "Opuntia ficus-indica",
        family: "Кактусовые (Cactaceae)",
        origin: "Мексика и Юго-Запад США",
        symbolism: "Выносливость, материнская забота, защита",
        description: "Плоские сегментированные кладодии с пучками золотистых глохидий и яркими цветками на макушке.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 196, 46),
            petal_shade: Rgb(232, 128, 32),
            center: Rgb(246, 72, 68),
            stem: Rgb(76, 174, 92),
            leaf: Rgb(54, 142, 72),
            accent: Rgb(255, 232, 134),
        },
        stem_start_row: 3,
        art: &[
            "           \\|/           \\|/           ",
            "          -(@)-         -(@)-          ",
            "          .---.         .---.          ",
            "         / * * \\       / * * \\         ",
            "        | *   * |     | *   * |        ",
            "         \\ * * / .---. \\ * * /         ",
            "          '-+-' / * * \\ '-+-'          ",
            "            \\  | *   * |  /            ",
            "             '-| *   * |-'             ",
            "                \\ * * /                ",
            "                 '---'                 ",
        ],
    },
    // 38. Алоэ Вера (Суккулент)
    Flower {
        name_ru: "Алоэ Вера",
        name_la: "Aloe barbadensis miller",
        family: "Асфоделовые (Asphodelaceae)",
        origin: "Аравийский полуостров",
        symbolism: "Исцеление, жизненная сила, долголетие",
        description: "Мясистая розетка мечевидных листьев с зубчатыми краями и высоким соцветием коралловых трубочек.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 118, 76),
            petal_shade: Rgb(224, 68, 48),
            center: Rgb(255, 214, 72),
            stem: Rgb(86, 182, 114),
            leaf: Rgb(62, 152, 92),
            accent: Rgb(178, 232, 188),
        },
        stem_start_row: 5,
        art: &[
            "                  .^.                  ",
            "                 ( * )                 ",
            "                ( *#* )                ",
            "                 ( * )                 ",
            "                   |                   ",
            "            |\\     |     /|            ",
            "           / *\\  |\\|/|  /* \\           ",
            "       |\\ |  * \\ | * | / *  | /|       ",
            "      / *\\ \\  * \\|   |/ *  / /* \\      ",
            "      \\  *\\ \\  * | * | *  / /*  /      ",
            "       '.__\\_'---'---'---'_/__.'       ",
        ],
    },
    // 39. Хавортия Полосатая (Суккулент)
    Flower {
        name_ru: "Хавортия Полосатая",
        name_la: "Haworthiopsis attenuata",
        family: "Асфоделовые (Asphodelaceae)",
        origin: "Восточно-Капская провинция, ЮАР",
        symbolism: "Стойкость, изящная геометрия, уют",
        description: "Компактная звёздчатая розетка заострённых изумрудных листьев с рельефными белыми полосками.",
        palette: FlowerPalette {
            petal_main: Rgb(238, 246, 255),
            petal_shade: Rgb(178, 204, 224),
            center: Rgb(255, 224, 118),
            stem: Rgb(54, 146, 84),
            leaf: Rgb(36, 116, 64),
            accent: Rgb(236, 248, 242),
        },
        stem_start_row: 4,
        art: &[
            "                   *                   ",
            "                  /|\\                  ",
            "                   |                   ",
            "              /\\   |   /\\              ",
            "             /**\\ /|\\ /**\\             ",
            "         /\\ /+**+V*+*V+**+\\ /\\         ",
            "        /**V+**+/*****\\+**+V**\\        ",
            "       /+**+**+/*******\\+**+**+\\       ",
            "       '._____/*********\\_____.'       ",
        ],
    },
    // 40. Литопс — Живой Камень (Суккулент)
    Flower {
        name_ru: "Литопс (Живой Камень)",
        name_la: "Lithops karasmontana",
        family: "Аизовые (Aizoaceae)",
        origin: "Пустыни Намибии и ЮАР",
        symbolism: "Маскировка, терпение, скрытая красота",
        description: "Суккулент-мимик из двух сросшихся мраморных листьев, из расщелины которых распускается яркий цветок.",
        palette: FlowerPalette {
            petal_main: Rgb(255, 212, 68),
            petal_shade: Rgb(232, 156, 36),
            center: Rgb(255, 118, 56),
            stem: Rgb(146, 158, 128),
            leaf: Rgb(134, 148, 116),
            accent: Rgb(186, 142, 118),
        },
        stem_start_row: 5,
        art: &[
            "                \\  |  /                ",
            "              -- ( @ ) --              ",
            "                /  |  \\                ",
            "                   |                   ",
            "                   |                   ",
            "         .-------. | .-------.         ",
            "       .' * + * + '\\/' + * + * '.       ",
            "      (  + * + * +  |  + * + * +  )      ",
            "       '._________.' '._________.'       ",
        ],
    },
];

/// ============================================================================
/// КОЛЛЕКЦИЯ ИЗ 10 ДЕТАЛИЗИРОВАННЫХ ВАЗ, ГОРШКОВ И КАШПО
/// (Все ёмкости выровнены по центральной оси: столбец 19)
/// ============================================================================
const VESSELS: &[Vessel] = &[
    // 1. Терракотовый горшок
    Vessel {
        name_ru: "Терракотовый Горшок",
        category: "Цветочный горшок",
        material: "Обожжённая глина",
        description: "Классический садовый горшок из тёплой пористой терракоты с рельефным ободком.",
        palette: VesselPalette {
            rim: Rgb(224, 126, 84),
            body_main: Rgb(198, 102, 66),
            body_shade: Rgb(154, 74, 46),
            pattern: Rgb(244, 178, 132),
        },
        art: &[
            "             .-----------.             ",
            "            [=============]            ",
            "             \\  ~  *  ~  /             ",
            "              \\   ~*~   /              ",
            "               \\_______/               ",
        ],
    },
    // 2. Хрустальная ваза
    Vessel {
        name_ru: "Хрустальная Ваза «Грация»",
        category: "Высокая ваза",
        material: "Огранённый горный хрусталь",
        description: "Прозрачная ваза с алмазной гранью, преломляющей свет в десятках искристых бликов.",
        palette: VesselPalette {
            rim: Rgb(224, 246, 255),
            body_main: Rgb(168, 218, 246),
            body_shade: Rgb(118, 174, 214),
            pattern: Rgb(255, 255, 255),
        },
        art: &[
            "              .---------.              ",
            "               \\  * *  /               ",
            "                )  *  (                ",
            "               / * * * \\               ",
            "              (  * * *  )              ",
            "               \\_______/               ",
            "              [=========]              ",
        ],
    },
    // 3. Фарфоровая ваза Мин
    Vessel {
        name_ru: "Фарфоровая Ваза Мин",
        category: "Восточная ваза",
        material: "Глазурованный кобальтовый фарфор",
        description: "Изящный сосуд с тонкой кобальтовой росписью по белоснежной глазури.",
        palette: VesselPalette {
            rim: Rgb(86, 146, 236),
            body_main: Rgb(242, 246, 255),
            body_shade: Rgb(186, 204, 232),
            pattern: Rgb(54, 108, 214),
        },
        art: &[
            "               .-------.               ",
            "                )  ~  (                ",
            "             .-'  ~*~  '-.             ",
            "            (   * @@@ *   )            ",
            "             '-.  ~*~  .-'             ",
            "               '-------'               ",
        ],
    },
    // 4. Японская чаша Суйбан
    Vessel {
        name_ru: "Чаша Суйбан (Икебана)",
        category: "Низкая чаша",
        material: "Чёрная глазурованная керамика",
        description: "Широкая плоская чаша для искусства икебаны с водной гладью и камешками.",
        palette: VesselPalette {
            rim: Rgb(124, 152, 164),
            body_main: Rgb(78, 96, 108),
            body_shade: Rgb(52, 66, 76),
            pattern: Rgb(146, 212, 232),
        },
        art: &[
            "         .-------------------.         ",
            "        (  ~  ~  ~ * ~  ~  ~  )        ",
            "         '-._______________.-'         ",
            "            '-------------'            ",
        ],
    },
    // 5. Медный кувшин
    Vessel {
        name_ru: "Медный Кувшин Антик",
        category: "Кувшин с ручкой",
        material: "Чеканная красная медь",
        description: "Старинный кувшин ручной ковки с тёплым металлическим отблеском и изогнутой ручкой.",
        palette: VesselPalette {
            rim: Rgb(246, 168, 108),
            body_main: Rgb(218, 126, 68),
            body_shade: Rgb(168, 88, 44),
            pattern: Rgb(255, 212, 156),
        },
        art: &[
            "              .---------.              ",
            "               \\  ~*~  /   .-.         ",
            "              .-'     '-. /   )        ",
            "             (   * o *   )   /         ",
            "              '-.     .-'---'          ",
            "                '-----'                ",
        ],
    },
    // 6. Греческая амфора
    Vessel {
        name_ru: "Греческая Амфора",
        category: "Античный сосуд",
        material: "Расписная терракота",
        description: "Античная двуручная амфора с геометрическим меандром и стройным основанием.",
        palette: VesselPalette {
            rim: Rgb(232, 154, 92),
            body_main: Rgb(196, 114, 62),
            body_shade: Rgb(142, 76, 38),
            pattern: Rgb(48, 38, 34),
        },
        art: &[
            "              .---------.              ",
            "            .-' \\     / '-.            ",
            "           (  (  ) ^ (  )  )           ",
            "            '-' / vvv \\ '-'            ",
            "               (  ~*~  )               ",
            "                \\     /                ",
            "               [=======]               ",
        ],
    },
    // 7. Стеклянный флорариум
    Vessel {
        name_ru: "Стеклянный Флорариум",
        category: "Геометрический террариум",
        material: "Витражное стекло и латунь",
        description: "Многогранный геометрический террариум в тонкой латунной оправе.",
        palette: VesselPalette {
            rim: Rgb(232, 196, 108),
            body_main: Rgb(164, 224, 212),
            body_shade: Rgb(112, 176, 166),
            pattern: Rgb(246, 222, 148),
        },
        art: &[
            "             .-----------.             ",
            "            / \\   * *   / \\            ",
            "           <   +-------+   >           ",
            "            \\ /  . * .  \\ /            ",
            "             '-----------'             ",
        ],
    },
    // 8. Плетёная корзинка
    Vessel {
        name_ru: "Плетёная Корзинка",
        category: "Плетёное кашпо",
        material: "Натуральный ротанг и лоза",
        description: "Уютное плетёное кашпо из золотистой ивовой лозы в деревенском стиле.",
        palette: VesselPalette {
            rim: Rgb(228, 186, 118),
            body_main: Rgb(196, 152, 86),
            body_shade: Rgb(152, 112, 58),
            pattern: Rgb(242, 208, 146),
        },
        art: &[
            "           .---------------.           ",
            "          ( # # # # # # # # )          ",
            "           \\ # # # # # # # /           ",
            "            '.___________.'            ",
        ],
    },
    // 9. Мраморный вазон
    Vessel {
        name_ru: "Мраморный Вазон",
        category: "Парковый вазон на ножке",
        material: "Резной каррарский мрамор",
        description: "Античная чаша на точеной ножке из светлого мрамора с тонкими прожилками.",
        palette: VesselPalette {
            rim: Rgb(246, 248, 250),
            body_main: Rgb(216, 222, 228),
            body_shade: Rgb(168, 176, 186),
            pattern: Rgb(144, 156, 168),
        },
        art: &[
            "          .-----------------.          ",
            "           '-.  ~  *  ~  .-'           ",
            "              '._______.'              ",
            "                 )   (                 ",
            "              .-'-----'-.              ",
            "             [===========]             ",
        ],
    },
    // 10. Деревянное кашпо
    Vessel {
        name_ru: "Деревянное Кашпо «Шале»",
        category: "Деревянная кадка",
        material: "Мореный дуб и кованые обручи",
        description: "Добротная дубовая кадка, стянутая тёмными металлическими обручами.",
        palette: VesselPalette {
            rim: Rgb(178, 124, 78),
            body_main: Rgb(144, 96, 56),
            body_shade: Rgb(106, 68, 38),
            pattern: Rgb(92, 102, 112),
        },
        art: &[
            "            .-------------.            ",
            "            |=============|            ",
            "            |  |  * *  |  |            ",
            "            |=============|            ",
            "            '-------------'            ",
        ],
    },
];

/// Формирует полный кадр для полноэкранного вывода в терминал
fn build_fullscreen_frame(
    flower_idx: usize,
    vessel_idx: usize,
    theme_idx: usize,
    auto_play: bool,
    animate: bool,
    show_controls: bool,
    hide_ui: bool,
    tick: usize,
    rows: usize,
    cols: usize,
) -> String {
    let theme = &THEMES[theme_idx % THEMES.len()];
    let flower = &FLOWERS[flower_idx % FLOWERS.len()];
    let vessel = &VESSELS[vessel_idx % VESSELS.len()];

    let mut out = String::with_capacity(rows * cols * 4);
    // Переводим курсор в левый верхний угол без очистки экрана (избегаем мерцания)
    out.push_str("\x1b[H");

    // Режим полного скрытия интерфейса (Дзен-режим: только цветок и ёмкость по центру экрана)
    if hide_ui {
        let total_art_h = flower.art.len() + vessel.art.len();
        let top_pad = rows.saturating_sub(total_art_h) / 2;
        let left_pad = cols.saturating_sub(CANVAS_WIDTH) / 2;
        let right_pad = cols.saturating_sub(CANVAS_WIDTH + left_pad);

        for r in 0..rows {
            out.push_str(&theme.bg_outer.bg());
            if r >= top_pad && r < top_pad + total_art_h {
                let art_r = r - top_pad;
                out.push_str(&" ".repeat(left_pad));
                if art_r < flower.art.len() {
                    out.push_str(&render_flower_line(
                        flower,
                        art_r,
                        theme.bg_outer,
                        animate,
                        tick,
                        flower_idx + vessel_idx,
                    ));
                } else {
                    let v_r = art_r - flower.art.len();
                    out.push_str(&render_vessel_line(vessel, v_r, theme.bg_outer));
                }
                out.push_str(&theme.bg_outer.bg());
                out.push_str(&" ".repeat(right_pad));
            } else {
                out.push_str(&" ".repeat(cols));
            }
            if r + 1 < rows {
                out.push_str("\r\n");
            }
        }
        out.push_str("\x1b[0m");
        return out;
    }

    let inner_w = cols.saturating_sub(2);
    let inner_h = rows.saturating_sub(2);

    if inner_w < 44 || inner_h < 18 {
        // Резервный компактный режим для сверхмалых окон терминала
        for r in 0..rows {
            out.push_str(&theme.bg_outer.bg());
            out.push_str(&theme.title.fg());
            let line = match r {
                1 => pad_center("FLORARIUM — ASCII ГАЛЕРЕЯ", cols),
                3 => pad_center(flower.name_ru, cols),
                4 => pad_center(vessel.name_ru, cols),
                6 => pad_center("Увеличьте окно терминала (мин. 46x20)", cols),
                7 => pad_center("[Space] Новый букет   [Q] Выход", cols),
                _ => " ".repeat(cols),
            };
            out.push_str(&line);
            if r + 1 < rows {
                out.push_str("\r\n");
            }
        }
        return out;
    }

    // Верхняя рамка окна
    out.push_str(&theme.bg_outer.bg());
    out.push_str(&theme.border.fg());
    out.push('╔');
    out.push_str(&"═".repeat(inner_w));
    out.push_str("╗\r\n");

    // Строка заголовка
    let combo_num = flower_idx * VESSELS.len() + vessel_idx + 1;
    let total_combos = FLOWERS.len() * VESSELS.len();
    let header_left = format!(" ✿ FLORARIUM  │  {} × {}", flower.name_ru, vessel.name_ru);
    let header_right = format!(
        "Букет #{:03}/{:03}  │  Тема: {} ",
        combo_num, total_combos, theme.name
    );

    let header_line = if visible_width(&header_left) + visible_width(&header_right) + 2 <= inner_w {
        let gap = inner_w - visible_width(&header_left) - visible_width(&header_right);
        format!(
            "{}{}{}{}{}{}{}",
            theme.title.fg(),
            "\x1b[1m",
            header_left,
            "\x1b[22m",
            " ".repeat(gap),
            theme.muted.fg(),
            header_right
        )
    } else {
        format!(
            "{}\x1b[1m{}\x1b[22m",
            theme.title.fg(),
            pad_center(&format!("✿ FLORARIUM — {}", flower.name_ru), inner_w)
        )
    };

    out.push_str(&theme.border.fg());
    out.push('║');
    out.push_str(&theme.bg_card.bg());
    out.push_str(&header_line);
    out.push_str(&theme.bg_outer.bg());
    out.push_str(&theme.border.fg());
    out.push_str("║\r\n");

    // Разделитель под заголовком
    out.push('╠');
    out.push_str(&"═".repeat(inner_w));
    out.push_str("╣\r\n");
    // Рабочая область: учитываем высоту шапки (3 строки) и наличие нижней панели подсказок (3 или 1 строка)
    let chrome_rows = if show_controls { 6 } else { 4 };
    let body_h = rows.saturating_sub(chrome_rows);
    let total_art_h = flower.art.len() + vessel.art.len();
    let side_by_side = inner_w >= 90 && body_h >= total_art_h + 2;

    if side_by_side {
        // Двухколоночный режим: слева сцена с растением, справа карточка гербария
        let stage_w = 47usize;
        let card_w = inner_w.saturating_sub(stage_w + 1); // 1 символ на разделитель '│'

        // Формируем строки карточки гербария
        let mut card_lines: Vec<String> = Vec::new();
        let inner_card = card_w.saturating_sub(4);

        card_lines.push(String::new());
        card_lines.push(format!(
            "  {}\x1b[1m◈ БОТАНИЧЕСКИЙ ПАСПОРТ ({:02}/{:02})\x1b[22m",
            theme.highlight.fg(),
            flower_idx + 1,
            FLOWERS.len()
        ));
        card_lines.push(format!("  {}{}", theme.border.fg(), "─".repeat(inner_card)));
        card_lines.push(format!(
            "  {}Вид:       {}\x1b[1m{}\x1b[22m",
            theme.muted.fg(),
            theme.title.fg(),
            flower.name_ru
        ));
        card_lines.push(format!(
            "  {}Латынь:    {}\x1b[3m{}\x1b[23m",
            theme.muted.fg(),
            theme.text.fg(),
            flower.name_la
        ));
        card_lines.push(format!(
            "  {}Семейство: {}{}",
            theme.muted.fg(),
            theme.text.fg(),
            flower.family
        ));
        card_lines.push(format!(
            "  {}Ареал:     {}{}",
            theme.muted.fg(),
            theme.text.fg(),
            flower.origin
        ));
        card_lines.push(String::new());

        for (idx, line) in wrap_text(&format!("Язык цветов: {}", flower.symbolism), inner_card)
            .into_iter()
            .enumerate()
        {
            let col = if idx == 0 {
                theme.highlight.fg()
            } else {
                theme.text.fg()
            };
            card_lines.push(format!("  {}{}", col, line));
        }

        card_lines.push(String::new());
        for line in wrap_text(flower.description, inner_card) {
            card_lines.push(format!("  {}{}", theme.text.fg(), line));
        }

        card_lines.push(String::new());
        card_lines.push(format!(
            "  {}\x1b[1m◈ ЁМКОСТЬ И ОФОРМЛЕНИЕ ({:02}/{:02})\x1b[22m",
            theme.highlight.fg(),
            vessel_idx + 1,
            VESSELS.len()
        ));
        card_lines.push(format!("  {}{}", theme.border.fg(), "─".repeat(inner_card)));
        card_lines.push(format!(
            "  {}Название:  {}\x1b[1m{}\x1b[22m",
            theme.muted.fg(),
            theme.title.fg(),
            vessel.name_ru
        ));
        card_lines.push(format!(
            "  {}Тип:       {}{}",
            theme.muted.fg(),
            theme.text.fg(),
            vessel.category
        ));
        card_lines.push(format!(
            "  {}Материал:  {}{}",
            theme.muted.fg(),
            theme.text.fg(),
            vessel.material
        ));
        for line in wrap_text(vessel.description, inner_card) {
            card_lines.push(format!("  {}{}", theme.muted.fg(), line));
        }

        // Цветовая палитра композиции
        card_lines.push(String::new());
        let swatches = format!(
            "  {}Палитра:   {}██ {}██ {}██ {}██ {}██ {}██",
            theme.muted.fg(),
            flower.palette.petal_main.fg(),
            flower.palette.petal_shade.fg(),
            flower.palette.center.fg(),
            flower.palette.stem.fg(),
            vessel.palette.body_main.fg(),
            vessel.palette.pattern.fg(),
        );
        card_lines.push(swatches);

        let art_top_pad = body_h.saturating_sub(total_art_h) / 2;
        let card_top_pad = body_h.saturating_sub(card_lines.len()) / 2;
        let stage_left_pad = (stage_w.saturating_sub(CANVAS_WIDTH)) / 2;
        let stage_right_pad = stage_w.saturating_sub(CANVAS_WIDTH + stage_left_pad);

        for r in 0..body_h {
            out.push_str(&theme.bg_outer.bg());
            out.push_str(&theme.border.fg());
            out.push('║');

            // Левая колонка: ASCII-сцена
            out.push_str(&theme.bg_outer.bg());
            if r >= art_top_pad && r < art_top_pad + total_art_h {
                let art_r = r - art_top_pad;
                out.push_str(&" ".repeat(stage_left_pad));
                if art_r < flower.art.len() {
                    out.push_str(&render_flower_line(
                        flower,
                        art_r,
                        theme.bg_outer,
                        animate,
                        tick,
                        flower_idx + vessel_idx,
                    ));
                } else {
                    let v_r = art_r - flower.art.len();
                    out.push_str(&render_vessel_line(vessel, v_r, theme.bg_outer));
                }
                out.push_str(&theme.bg_outer.bg());
                out.push_str(&" ".repeat(stage_right_pad));
            } else {
                out.push_str(&" ".repeat(stage_w));
            }

            // Вертикальный разделитель между сценой и карточкой
            out.push_str(&theme.border.fg());
            out.push('│');

            // Правая колонка: карточка гербария
            out.push_str(&theme.bg_card.bg());
            if r >= card_top_pad && r < card_top_pad + card_lines.len() {
                let c_line = &card_lines[r - card_top_pad];
                out.push_str(&pad_right(c_line, card_w));
            } else {
                out.push_str(&" ".repeat(card_w));
            }

            out.push_str(&theme.bg_outer.bg());
            out.push_str(&theme.border.fg());
            out.push_str("║\r\n");
        }
    } else {
        // Компактный вертикальный режим для узких терминалов
        let info_h = 3;
        let avail_art_h = body_h.saturating_sub(info_h);
        let top_pad = avail_art_h.saturating_sub(total_art_h) / 2;
        let left_pad = inner_w.saturating_sub(CANVAS_WIDTH) / 2;
        let right_pad = inner_w.saturating_sub(CANVAS_WIDTH + left_pad);

        for r in 0..body_h {
            out.push_str(&theme.bg_outer.bg());
            out.push_str(&theme.border.fg());
            out.push('║');
            out.push_str(&theme.bg_outer.bg());

            if r >= top_pad && r < top_pad + total_art_h {
                let art_r = r - top_pad;
                out.push_str(&" ".repeat(left_pad));
                if art_r < flower.art.len() {
                    out.push_str(&render_flower_line(
                        flower,
                        art_r,
                        theme.bg_outer,
                        animate,
                        tick,
                        flower_idx + vessel_idx,
                    ));
                } else {
                    let v_r = art_r - flower.art.len();
                    out.push_str(&render_vessel_line(vessel, v_r, theme.bg_outer));
                }
                out.push_str(&theme.bg_outer.bg());
                out.push_str(&" ".repeat(right_pad));
            } else if r == body_h.saturating_sub(3) {
                let label = format!("{} ({})", flower.name_ru, flower.name_la);
                out.push_str(&theme.title.fg());
                out.push_str(&pad_center(&label, inner_w));
            } else if r == body_h.saturating_sub(2) {
                out.push_str(&theme.muted.fg());
                out.push_str(&pad_center(vessel.name_ru, inner_w));
            } else {
                out.push_str(&" ".repeat(inner_w));
            }

            out.push_str(&theme.bg_outer.bg());
            out.push_str(&theme.border.fg());
            out.push_str("║\r\n");
        }
    }

    if show_controls {
        // Нижний разделитель и панель управления
        out.push_str(&theme.bg_outer.bg());
        out.push_str(&theme.border.fg());
        out.push('╠');
        out.push_str(&"═".repeat(inner_w));
        out.push_str("╣\r\n");

        let auto_badge = if auto_play { "ВКЛ" } else { "ВЫКЛ" };
        let motion_badge = if animate { "ВКЛ" } else { "ВЫКЛ" };
        let controls = format!(
            "[Space] Случайный  [F] Цветок  [V] Ваза  [←/→] Листать  [A] Авто:{}  [T] Тема  [M] Пыльца:{}  [H] Подсказки  [U] Без UI  [Q] Выход",
            auto_badge, motion_badge
        );
        let compact_controls = "[Space] Случ. [F] Цветок [V] Ваза [T] Тема [H] Подск. [U] Без UI [Q] Выход";
        let mini_controls = "[Space] Новый  [F/V] Выбор  [H] Подск.  [U] Без UI  [Q] Выход";

        let footer_text = if visible_width(&controls) <= inner_w {
            pad_center(&controls, inner_w)
        } else if visible_width(compact_controls) <= inner_w {
            pad_center(compact_controls, inner_w)
        } else {
            pad_center(mini_controls, inner_w)
        };

        out.push('║');
        out.push_str(&theme.bg_card.bg());
        out.push_str(&theme.text.fg());
        out.push_str(&footer_text);
        out.push_str(&theme.bg_outer.bg());
        out.push_str(&theme.border.fg());
        out.push_str("║\r\n");
    }

    // Нижняя рамка окна
    out.push_str(&theme.bg_outer.bg());
    out.push_str(&theme.border.fg());
    out.push('╚');
    out.push_str(&"═".repeat(inner_w));
    out.push('╝');
    out.push_str("\x1b[0m");

    out
}

/// Выводит один случайный цветок в вазе в обычный поток вывода (без полноэкранного режима)
fn print_single_bouquet(flower_idx: usize, vessel_idx: usize) {
    let flower = &FLOWERS[flower_idx % FLOWERS.len()];
    let vessel = &VESSELS[vessel_idx % VESSELS.len()];
    let bg = Rgb(16, 20, 26);

    println!();
    for r in 0..flower.art.len() {
        println!("  {}\x1b[0m", render_flower_line(flower, r, bg, false, 0, 0));
    }
    for r in 0..vessel.art.len() {
        println!("  {}\x1b[0m", render_vessel_line(vessel, r, bg));
    }
    println!(
        "\n  \x1b[1;38;2;132;235;198m{}\x1b[0m (\x1b[3m{}\x1b[0m)",
        flower.name_ru, flower.name_la
    );
    println!("  \x1b[38;2;180;196;212mЁмкость: {}\x1b[0m\n", vessel.name_ru);
}

/// Создаёт готовую директорию проекта с `shell.nix`, `Cargo.toml`, `src/main.rs`
/// и упаковывает её в `florarium.zip`.
fn create_nix_zip_bundle() -> io::Result<()> {
    let project_dir = "florarium_nix";
    let src_dir = format!("{}/src", project_dir);
    fs::create_dir_all(&src_dir)?;

    let shell_nix = r#"{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  name = "florarium-ascii-env";

  buildInputs = with pkgs; [
    rustc
    cargo
    ncurses
  ];

  shellHook = ''
    echo "===================================================="
    echo " Florarium ASCII — окружение Nix готово к запуску!"
    echo " Сборка и запуск: cargo run --release"
    echo "===================================================="
  '';
}
"#;

    let cargo_toml = r#"[package]
name = "florarium"
version = "1.0.0"
edition = "2021"
description = "Полноэкранный генератор случайных ASCII-цветов в вазах и горшках"

[dependencies]
"#;

    let source_code = include_str!(file!());

    fs::write(format!("{}/shell.nix", project_dir), shell_nix)?;
    fs::write(format!("{}/Cargo.toml", project_dir), cargo_toml)?;
    fs::write(format!("{}/main.rs", src_dir), source_code)?;

    let zip_status = Command::new("zip")
        .args(["-r", "florarium.zip", project_dir])
        .status();

    match zip_status {
        Ok(st) if st.success() => {
            println!("Архив florarium.zip успешно создан (включает shell.nix, Cargo.toml и src/main.rs).");
        }
        _ => {
            println!(
                "Каталог {}/ создан с shell.nix и Cargo.toml. Для упаковки выполните: zip -r florarium.zip {}",
                project_dir, project_dir
            );
        }
    }
    Ok(())
}

/// Точка входа в приложение
fn main() -> io::Result<()> {
    // Гарантируем восстановление терминала даже при непредвиденной панике
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = write!(out, "\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        let _ = Command::new("stty")
            .arg("sane")
            .stdin(Stdio::inherit())
            .status();
        default_hook(info);
    }));

    let mut rng = Rng::from_entropy();
    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Florarium — консольная галерея ASCII-цветов в вазах и горшках");
        println!("Использование:");
        println!("  florarium            Открыть полноэкранную интерактивную галерею");
        println!("  florarium --print    Вывести один случайный букет в обычный терминал");
        println!("  florarium --list     Показать полный каталог (40 растений и 10 ёмкостей)");
        println!("  florarium --bundle   Создать проект с shell.nix и упаковать в florarium.zip");
        return Ok(());
    }

    if args.iter().any(|a| a == "--list") {
        println!("=== КОЛЛЕКЦИЯ РАСТЕНИЙ ({} видов) ===", FLOWERS.len());
        for (i, f) in FLOWERS.iter().enumerate() {
            println!("  {:02}. {} ({})", i + 1, f.name_ru, f.name_la);
        }
        println!("\n=== КОЛЛЕКЦИЯ ЁМКОСТЕЙ ({} видов) ===", VESSELS.len());
        for (i, v) in VESSELS.iter().enumerate() {
            println!("  {:02}. {} — {}", i + 1, v.name_ru, v.material);
        }
        return Ok(());
    }

    if args.iter().any(|a| a == "--bundle") {
        return create_nix_zip_bundle();
    }

    let mut flower_idx = rng.gen_range(FLOWERS.len());
    let mut vessel_idx = rng.gen_range(VESSELS.len());

    if args.iter().any(|a| a == "--print") {
        print_single_bouquet(flower_idx, vessel_idx);
        return Ok(());
    }

    // Полноэкранный интерактивный режим
    let _guard = TerminalGuard::enter_fullscreen()?;
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    let mut theme_idx = 0usize;
    let mut auto_play = false;
    let mut animate = true;
    let mut show_controls = true;
    let mut controls_manually_toggled = false;
    let mut hide_ui = false;
    let mut tick = 0usize;
    let app_start = Instant::now();
    let mut last_auto_switch = Instant::now();
    let mut last_size = (0usize, 0usize);

    let mut buf = [0u8; 16];

    loop {
        let (rows, cols) = get_terminal_size();
        if (rows, cols) != last_size {
            write!(stdout, "\x1b[2J\x1b[H")?;
            last_size = (rows, cols);
        }

        // Автоматически скрываем подсказки управления через 5 секунд после запуска
        if !controls_manually_toggled
            && show_controls
            && app_start.elapsed() >= Duration::from_secs(5)
        {
            show_controls = false;
        }

        if auto_play && last_auto_switch.elapsed() >= Duration::from_secs(4) {
            flower_idx = rng.gen_range(FLOWERS.len());
            vessel_idx = rng.gen_range(VESSELS.len());
            last_auto_switch = Instant::now();
        }

        let frame = build_fullscreen_frame(
            flower_idx,
            vessel_idx,
            theme_idx,
            auto_play,
            animate,
            show_controls,
            hide_ui,
            tick,
            rows,
            cols,
        );
        stdout.write_all(frame.as_bytes())?;
        stdout.flush()?;

        // Читаем ввод с клавиатуры (stty обеспечивает неблокирующее чтение ~100 мс)
        if let Ok(n) = stdin.read(&mut buf) {
            if n > 0 {
                // Обработка стрелок ESC [ A/B/C/D
                if n >= 3 && buf[0] == 0x1b && buf[1] == b'[' {
                    match buf[2] {
                        b'C' => flower_idx = (flower_idx + 1) % FLOWERS.len(),
                        b'D' => flower_idx = (flower_idx + FLOWERS.len() - 1) % FLOWERS.len(),
                        b'A' => vessel_idx = (vessel_idx + 1) % VESSELS.len(),
                        b'B' => vessel_idx = (vessel_idx + VESSELS.len() - 1) % VESSELS.len(),
                        _ => {}
                    }
                    last_auto_switch = Instant::now();
                } else if n == 1 && buf[0] == 0x1b {
                    // Одиночный Esc — выход
                    break;
                } else {
                    match buf[0] {
                        b'q' | b'Q' | 3 => break, // Q или Ctrl+C
                        b' ' | b'r' | b'R' | b'\r' | b'\n' => {
                            let mut next_f = rng.gen_range(FLOWERS.len());
                            if FLOWERS.len() > 1 && next_f == flower_idx {
                                next_f = (next_f + 1) % FLOWERS.len();
                            }
                            flower_idx = next_f;
                            vessel_idx = rng.gen_range(VESSELS.len());
                            last_auto_switch = Instant::now();
                        }
                        b'f' | b'F' => {
                            flower_idx = (flower_idx + 1 + rng.gen_range(FLOWERS.len() - 1))
                                % FLOWERS.len();
                            last_auto_switch = Instant::now();
                        }
                        b'v' | b'V' => {
                            vessel_idx = (vessel_idx + 1 + rng.gen_range(VESSELS.len() - 1))
                                % VESSELS.len();
                            last_auto_switch = Instant::now();
                        }
                        b'a' | b'A' => {
                            auto_play = !auto_play;
                            last_auto_switch = Instant::now();
                        }
                        b't' | b'T' => {
                            theme_idx = (theme_idx + 1) % THEMES.len();
                        }
                        b'm' | b'M' => {
                            animate = !animate;
                        }
                        b'h' | b'H' | b'?' => {
                            show_controls = !show_controls;
                            controls_manually_toggled = true;
                        }
                        b'u' | b'U' | b'\t' => {
                            hide_ui = !hide_ui;
                        }
                        _ => {}
                    }
                }
            }
        }

        tick = tick.wrapping_add(1);
    }

    Ok(())
}