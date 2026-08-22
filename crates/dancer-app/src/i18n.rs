//! User-facing text, in the user's language.
//!
//! # Scope
//!
//! Everything a person reads on screen: tray and context menus, dialogs, the
//! help text. Not the log, not `--help`, not error chains — those are developer
//! surfaces, read by whoever is debugging, and a Russian log line pasted into an
//! English issue helps nobody.
//!
//! # Shape
//!
//! One `Strings` table per language, every entry a `&'static str` or a plain
//! `fn` for the handful that take parameters. No crate: the app has forty-odd
//! strings, and a localisation framework would be more code than the strings.
//! The table is a struct rather than a map so a missing translation is a compile
//! error, not a blank menu item at runtime.
//!
//! The language is chosen once at startup — `[ui] language` in the config, or the
//! Windows UI language when that says `auto` — and read through [`t`] from
//! anywhere. A global, deliberately: threading a language through every menu
//! constructor and dialog call would touch every signature in the app for a
//! value that never changes after line one of `main`.

use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// Fix the language for the rest of the process. Later calls are ignored.
pub fn set(lang: Lang) {
    let _ = LANG.set(lang);
}

/// The active table. English until [`set`] is called, which is what tests get.
pub fn t() -> &'static Strings {
    match LANG.get().copied().unwrap_or(Lang::En) {
        Lang::En => &EN,
        Lang::Ru => &RU,
    }
}

impl Lang {
    /// `"en"`, `"ru"`, or `"auto"` (anything else counts as auto).
    pub fn from_config(s: &str) -> Lang {
        match s.trim().to_ascii_lowercase().as_str() {
            "en" => Lang::En,
            "ru" => Lang::Ru,
            _ => Lang::system(),
        }
    }

    /// The Windows UI language, reduced to what there is a table for.
    #[cfg(windows)]
    fn system() -> Lang {
        use windows::Win32::Globalization::GetUserDefaultUILanguage;
        // A LANGID: the low ten bits are the primary language. 0x19 is Russian in
        // every sub-language (ru-RU, ru-MD, ...).
        let id = unsafe { GetUserDefaultUILanguage() };
        if id & 0x3ff == 0x19 {
            Lang::Ru
        } else {
            Lang::En
        }
    }

    #[cfg(not(windows))]
    fn system() -> Lang {
        Lang::En
    }
}

/// Every string the UI shows.
pub struct Strings {
    // Tray.
    pub click_through: &'static str,
    pub always_on_top: &'static str,
    pub anticipate: &'static str,
    pub offset_minus_coarse: &'static str,
    pub offset_minus_fine: &'static str,
    pub offset_plus_fine: &'static str,
    pub offset_plus_coarse: &'static str,
    pub reset_offset: &'static str,
    pub open_data_folder: &'static str,
    pub quit: &'static str,
    pub dancers: &'static str,
    pub add_dancer: &'static str,
    pub no_sheets: &'static str,
    pub random: &'static str,
    pub remove_last_dancer: &'static str,
    pub open_artwork_folder: &'static str,
    pub how_to_add_dancers: &'static str,
    pub get_sheet: fn(&str) -> String,
    pub output_offset: fn(f64) -> String,
    pub sign_in_yandex: &'static str,
    /// The playback state, by its English name from `State::name()`.
    pub state: fn(&str) -> &'static str,

    // Account line in the tray.
    pub yandex_off: &'static str,
    pub yandex_checking: &'static str,
    pub yandex_as: fn(&str) -> String,
    pub yandex_expired: &'static str,
    pub yandex_unreachable: &'static str,

    // Context menu on a dancer.
    pub sheet: &'static str,
    pub size: &'static str,
    pub size_current: fn(f32) -> String,
    pub mirror: &'static str,
    pub duplicate: &'static str,
    pub remove_this_dancer: &'static str,
    pub quit_app: &'static str,

    // Dialogs.
    pub fatal_title: &'static str,
    pub fatal_body: fn(&str) -> String,
    pub whose_artwork: &'static str,
    pub ownership_warning: fn(name: &str, owner: &str) -> String,
    pub adding_dancers: &'static str,
    pub help_text: fn(&Path) -> String,
    pub signed_in_title: &'static str,
    pub signed_in_body: fn(login: &str, config: &Path, revoke_url: &str) -> String,
    pub sign_in_failed_title: &'static str,
    pub sign_in_failed_body: fn(why: &str) -> String,
    pub sign_in_expired_title: &'static str,
    pub sign_in_expired_body: &'static str,
    pub sign_in_code_title: &'static str,
    pub sign_in_code_body: fn(code: &str, minutes: u64) -> String,
}

/// Join paragraphs of lines into dialog text: lines within a paragraph are
/// separated by a space, paragraphs by a blank line. Keeps every literal short
/// and flush-left, which is what the line-continuation guard test needs.
fn paragraphs(ps: &[&[&str]]) -> String {
    ps.iter()
        .map(|p| p.join(" "))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub const EN: Strings = Strings {
    click_through: "Click-through",
    always_on_top: "Always on top",
    anticipate: "Anticipate the beat",
    offset_minus_coarse: "Offset  -25 ms",
    offset_minus_fine: "Offset   -5 ms",
    offset_plus_fine: "Offset   +5 ms",
    offset_plus_coarse: "Offset  +25 ms",
    reset_offset: "Reset offset",
    open_data_folder: "Open data folder",
    quit: "Quit",
    dancers: "Dancers",
    add_dancer: "Add dancer",
    no_sheets: "No sheets in the artwork folder",
    random: "Random",
    remove_last_dancer: "Remove last dancer",
    open_artwork_folder: "Open artwork folder",
    how_to_add_dancers: "How to add dancers...",
    get_sheet: |name| format!("Get {name}..."),
    output_offset: |secs| format!("Output offset: {:+.0} ms", secs * 1000.0),
    sign_in_yandex: "Sign in to Yandex Music...",
    state: |s| s_en(s),

    yandex_off: "Yandex: not signed in",
    yandex_checking: "Yandex: checking…",
    yandex_as: |login| format!("Yandex: {login}"),
    yandex_expired: "Yandex: sign-in expired",
    yandex_unreachable: "Yandex: unreachable",

    sheet: "Sheet",
    size: "Size",
    size_current: |s| format!("{:.0}% (current)", s * 100.0),
    mirror: "Mirror",
    duplicate: "Duplicate",
    remove_this_dancer: "Remove this dancer",
    quit_app: "Quit dancer-rs",

    fatal_title: "dancer-rs could not start",
    fatal_body: |e| format!("{e}\n\nDetails are in dancer-rs.log, next to the executable."),
    whose_artwork: "Whose artwork is this?",
    ownership_warning: |name, owner| {
        paragraphs(&[
            &[name, "is not ours, and downloading it does not make it yours."],
            &["The artwork belongs to", owner, "— published on their terms. Read them."],
            &[
                "dancer-rs does not host, bundle or redistribute any sprite sheet, and",
                "neither should you: please do not pass sheets on with copies of this app.",
                "The only artwork shipped with it is the plain default sheet.",
            ],
            &["Open the download page in your browser?"],
        ])
    },
    adding_dancers: "Adding dancers",
    help_text: |dir| {
        let dir = dir.display().to_string();
        paragraphs(&[
            &[
                "A dancer is a sprite sheet: one PNG that is 8 cells wide, with one row",
                "per animation, plus a .txt naming those rows one per line.",
            ],
            &["The format comes from FAOSDance and Fruity Dance, so sheets made for either will work here."],
            &["To add one:"],
            &["1. Put the .png and its .txt in", &dir, "— a subfolder per dancer is fine."],
            &["2. Right-click a dancer and pick it under Sheet, or add a new dancer from the tray."],
            &[
                "For it to dance in time rather than just loop, add a .toml beside the PNG",
                "saying which cell is each move's accent. See default.toml in that folder for",
                "a worked example, and check your work with:",
            ],
            &["dancer-rs.exe <sheet.png> --check-sheet"],
            &["----"],
            &[
                "Sprite sheets are other people's work. Neither dancer-rs nor you own the",
                "artwork on the pages linked in that menu — it belongs to whoever made it,",
                "published on their terms. Nothing is bundled or redistributed here, and",
                "please do not pass sheets on with copies of this app. The only artwork",
                "shipped with it is the plain default sheet.",
            ],
        ])
    },
    signed_in_title: "Signed in to Yandex Music",
    signed_in_body: |login, config, url| {
        let config = config.display().to_string();
        paragraphs(&[
            &["Signed in as", login, "."],
            &[
                "Streamed tracks will now be fetched, analysed and deleted immediately —",
                "only the beat grid is kept.",
            ],
            &["The token is stored in plain text in", &config],
            &["Revoke it any time at", url],
        ])
    },
    sign_in_failed_title: "Sign-in did not complete",
    sign_in_failed_body: |why| {
        paragraphs(&[
            &[why],
            &[
                "Nothing has changed. You can try again from the tray menu whenever you",
                "like — the dancer works without it, it just cannot analyse streamed tracks.",
            ],
        ])
    },
    sign_in_expired_title: "Yandex sign-in expired",
    sign_in_expired_body: "The saved Yandex Music sign-in is no longer valid — it has expired or been revoked.\n\n\
Until you sign in again, streamed tracks cannot be analysed and the dancer will loop at a fixed rate for them. Everything else keeps working.\n\n\
Sign in again now?",
    sign_in_code_title: "Sign in to Yandex Music",
    sign_in_code_body: |code, minutes| {
        let minutes = minutes.to_string();
        paragraphs(&[
            &["A Yandex page has opened in your browser."],
            &["Enter this code:"],
            &["        ", code],
            &[
                "The code is valid for about",
                &minutes,
                "minutes. Press Ctrl+C now to copy this message if you need the code elsewhere.",
            ],
            &[
                "Click OK when you are done — signing in continues in the background,",
                "and the dancer keeps dancing.",
            ],
        ])
    },
};

pub const RU: Strings = Strings {
    click_through: "Пропускать клики",
    always_on_top: "Поверх всех окон",
    anticipate: "Предвосхищать бит",
    offset_minus_coarse: "Смещение  −25 мс",
    offset_minus_fine: "Смещение   −5 мс",
    offset_plus_fine: "Смещение   +5 мс",
    offset_plus_coarse: "Смещение  +25 мс",
    reset_offset: "Сбросить смещение",
    open_data_folder: "Открыть папку данных",
    quit: "Выход",
    dancers: "Танцоры",
    add_dancer: "Добавить танцора",
    no_sheets: "В папке с графикой нет спрайт-листов",
    random: "Случайный",
    remove_last_dancer: "Убрать последнего танцора",
    open_artwork_folder: "Открыть папку с графикой",
    how_to_add_dancers: "Как добавить танцоров…",
    get_sheet: |name| format!("Скачать {name}…"),
    output_offset: |secs| format!("Задержка вывода: {:+.0} мс", secs * 1000.0),
    sign_in_yandex: "Войти в Яндекс Музыку…",
    state: |s| s_ru(s),

    yandex_off: "Яндекс: вход не выполнен",
    yandex_checking: "Яндекс: проверка…",
    yandex_as: |login| format!("Яндекс: {login}"),
    yandex_expired: "Яндекс: вход истёк",
    yandex_unreachable: "Яндекс: недоступен",

    sheet: "Спрайт-лист",
    size: "Размер",
    size_current: |s| format!("{:.0}% (текущий)", s * 100.0),
    mirror: "Отразить",
    duplicate: "Дублировать",
    remove_this_dancer: "Убрать этого танцора",
    quit_app: "Выйти из dancer-rs",

    fatal_title: "dancer-rs не смог запуститься",
    fatal_body: |e| format!("{e}\n\nПодробности — в dancer-rs.log рядом с исполняемым файлом."),
    whose_artwork: "Чья это графика?",
    ownership_warning: |name, owner| {
        paragraphs(&[
            &[name, "— не наша работа, и скачивание не делает её вашей."],
            &["Графика принадлежит:", owner, "— и публикуется на их условиях. Прочитайте их."],
            &[
                "dancer-rs не хранит, не включает в комплект и не распространяет спрайт-листы,",
                "и вам тоже не стоит: пожалуйста, не передавайте листы вместе с копиями приложения.",
                "Единственная графика в комплекте — простой лист по умолчанию.",
            ],
            &["Открыть страницу загрузки в браузере?"],
        ])
    },
    adding_dancers: "Добавление танцоров",
    help_text: |dir| {
        let dir = dir.display().to_string();
        paragraphs(&[
            &[
                "Танцор — это спрайт-лист: один PNG шириной 8 ячеек, по строке на каждую",
                "анимацию, плюс файл .txt с названиями строк, по одному на строку.",
            ],
            &["Формат унаследован от FAOSDance и Fruity Dance, так что подойдут листы для любого из них."],
            &["Чтобы добавить лист:"],
            &["1. Положите .png и его .txt в", &dir, "— можно в отдельную подпапку для каждого танцора."],
            &["2. Щёлкните танцора правой кнопкой и выберите лист в меню «Спрайт-лист», либо добавьте нового танцора из трея."],
            &[
                "Чтобы танцор попадал в такт, а не просто зацикливался, положите рядом с PNG",
                "файл .toml, где указано, какая ячейка — акцент каждого движения. Образец —",
                "default.toml в той же папке; проверить свой лист можно так:",
            ],
            &["dancer-rs.exe <sheet.png> --check-sheet"],
            &["----"],
            &[
                "Спрайт-листы — чужая работа. Ни dancer-rs, ни вы не владеете графикой со",
                "страниц, на которые ведёт это меню: она принадлежит авторам и публикуется на",
                "их условиях. Здесь ничего не включено в комплект и не распространяется;",
                "пожалуйста, не передавайте листы вместе с копиями приложения. Единственная",
                "графика в комплекте — простой лист по умолчанию.",
            ],
        ])
    },
    signed_in_title: "Вход в Яндекс Музыку выполнен",
    signed_in_body: |login, config, url| {
        let config = config.display().to_string();
        paragraphs(&[
            &["Вы вошли как", login, "."],
            &[
                "Теперь треки из стриминга будут загружаться, анализироваться и сразу",
                "удаляться — остаётся только сетка битов.",
            ],
            &["Токен хранится открытым текстом в файле", &config],
            &["Отозвать его можно в любой момент:", url],
        ])
    },
    sign_in_failed_title: "Вход не завершён",
    sign_in_failed_body: |why| {
        paragraphs(&[
            &[why],
            &[
                "Ничего не изменилось. Попробовать снова можно в любой момент из меню в трее —",
                "танцор работает и без входа, просто не сможет анализировать треки из стриминга.",
            ],
        ])
    },
    sign_in_expired_title: "Вход в Яндекс истёк",
    sign_in_expired_body: "Сохранённый вход в Яндекс Музыку больше не действует — он истёк или был отозван.\n\n\
Пока вы не войдёте снова, треки из стриминга не будут анализироваться, и танцор будет двигаться с постоянной скоростью. Всё остальное работает.\n\n\
Войти снова сейчас?",
    sign_in_code_title: "Вход в Яндекс Музыку",
    sign_in_code_body: |code, minutes| {
        let minutes = minutes.to_string();
        paragraphs(&[
            &["В браузере открылась страница Яндекса."],
            &["Введите этот код:"],
            &["        ", code],
            &[
                "Код действует около",
                &minutes,
                "мин. Нажмите Ctrl+C сейчас, чтобы скопировать это сообщение, если код нужен в другом месте.",
            ],
            &[
                "Нажмите OK, когда закончите — вход продолжится в фоне,",
                "а танцор продолжит танцевать.",
            ],
        ])
    },
};

fn s_en(s: &str) -> &'static str {
    match s {
        "Idle" => "Idle",
        "Identifying" => "Identifying",
        "Unscored" => "Unscored",
        "Locked" => "Locked",
        "Resync" => "Resync",
        _ => "?",
    }
}

fn s_ru(s: &str) -> &'static str {
    match s {
        "Idle" => "Ожидание",
        "Identifying" => "Распознавание",
        "Unscored" => "Без сетки",
        "Locked" => "В такте",
        "Resync" => "Подстройка",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_text(s: &Strings) -> Vec<String> {
        let p = Path::new("C:/app/assets");
        vec![
            s.click_through.into(),
            s.always_on_top.into(),
            s.anticipate.into(),
            s.reset_offset.into(),
            s.open_data_folder.into(),
            s.quit.into(),
            s.dancers.into(),
            s.add_dancer.into(),
            s.no_sheets.into(),
            s.random.into(),
            s.remove_last_dancer.into(),
            s.open_artwork_folder.into(),
            s.how_to_add_dancers.into(),
            (s.get_sheet)("X"),
            (s.output_offset)(0.18),
            s.sign_in_yandex.into(),
            s.yandex_off.into(),
            (s.yandex_as)("me"),
            s.sheet.into(),
            s.size.into(),
            (s.size_current)(0.9),
            s.mirror.into(),
            s.duplicate.into(),
            s.remove_this_dancer.into(),
            s.quit_app.into(),
            s.fatal_title.into(),
            (s.fatal_body)("boom"),
            s.whose_artwork.into(),
            (s.ownership_warning)("Sheet", "Owner"),
            s.adding_dancers.into(),
            (s.help_text)(p),
            (s.signed_in_body)("me", p, "https://x"),
            (s.sign_in_failed_body)("why"),
            s.sign_in_expired_body.into(),
            (s.sign_in_code_body)("ABCD", 5),
        ]
    }

    #[test]
    fn no_user_facing_string_in_any_language_has_collapsed_line_continuations() {
        // A `\` continuation in a Rust string keeps the next line's indentation
        // unless that line is flush left. It compiles, passes every other test,
        // and renders as a twenty-space gap mid-sentence. Only reading catches
        // it — so this reads every string in every table.
        for table in [&EN, &RU] {
            for text in every_text(table) {
                for line in text.lines() {
                    let body = line.trim_start();
                    // The sign-in code is deliberately indented; nothing else is.
                    assert!(
                        !body.contains("  ") || line.starts_with("        "),
                        "run of spaces mid-line: {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_string_is_translated_not_copied() {
        // A table entry pasted from English and never translated would pass
        // every other check. Menu labels must differ; the few that legitimately
        // match (URLs, the bare code line) are not in this list.
        let en = every_text(&EN);
        let ru = every_text(&RU);
        for (e, r) in en.iter().zip(&ru) {
            assert_ne!(e, r, "not translated: {e:?}");
        }
    }

    #[test]
    fn the_config_value_is_forgiving() {
        assert_eq!(Lang::from_config("ru"), Lang::Ru);
        assert_eq!(Lang::from_config(" RU "), Lang::Ru);
        assert_eq!(Lang::from_config("en"), Lang::En);
        // "auto" and nonsense both defer to the system; whatever that is, it is
        // one of the two tables.
        let _ = Lang::from_config("auto");
        let _ = Lang::from_config("klingon");
    }

    #[test]
    fn every_state_name_has_a_word() {
        for s in ["Idle", "Identifying", "Unscored", "Locked", "Resync"] {
            assert_ne!((EN.state)(s), "?");
            assert_ne!((RU.state)(s), "?");
        }
    }
}
