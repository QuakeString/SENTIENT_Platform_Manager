// Copyright © 2016-2026 The SENTIENT Authors

//! The Platform Manager in the person's language: the 64 languages SENTIENT's
//! own UI ships (ui-ngx/src/assets/locale).
//!
//! Texts are written in English in the code, in `t!("..")` (a `&'static str`)
//! or `tf!("..", args)` (as `format!`). English is the key: `i18n/<code>.json`
//! maps each English text to its translation, with the placeholders written
//! `{0}`, `{1}` or `{name}`. A text a language does not have yet shows in
//! English. `tools/i18n` finds the texts and writes `i18n/en.json`, the list
//! every translation is checked against.
//!
//! What stays English: logs, the support file, the server agent's own output
//! (it runs where no language is chosen), names, ids, paths, versions and
//! commands. Numbers in a translated text use the language's own digits where
//! SENTIENT's UI does (Bengali ০১২, Marathi १२३, Persian ۱۲۳ ...); a value
//! that is not a plain number (a version, an address, a name) keeps its digits.
//!
//! The language is read once at start (Prefs.language, else the system's)
//! and can change while the window is open: views read `t!` as they render,
//! so the next frame is in the new language. A text built once and kept (a
//! job's plan, a message already shown) stays in the language it was made in.

use std::collections::HashMap;
use std::sync::RwLock;

/// One UI language.
#[derive(Debug, Clone, Copy)]
pub struct Language {
    /// SENTIENT's locale code: `hi_IN`, `zh_TW`.
    pub code: &'static str,
    /// Its own name, as the picker shows it.
    pub native: &'static str,
    pub english: &'static str,
    /// Written right to left.
    pub rtl: bool,
    /// Its digits 0-9, when it does not write 0-9 (as SENTIENT's UI, Intl).
    pub digits: Option<&'static str>,
}

const fn l(code: &'static str, native: &'static str, english: &'static str) -> Language {
    Language { code, native, english, rtl: false, digits: None }
}
const fn rtl(code: &'static str, native: &'static str, english: &'static str, digits: Option<&'static str>) -> Language {
    Language { code, native, english, rtl: true, digits }
}
const fn dg(code: &'static str, native: &'static str, english: &'static str, digits: &'static str) -> Language {
    Language { code, native, english, rtl: false, digits: Some(digits) }
}

const BENGALI: &str = "০১২৩৪৫৬৭৮৯";
const DEVANAGARI: &str = "०१२३४५६७८९";
const PERSIAN: &str = "۰۱۲۳۴۵۶۷۸۹";

/// Every language, English first, then by English name.
pub const LANGUAGES: &[Language] = &[
    l("en_US", "English", "English"),
    rtl("ar_AE", "العربية", "Arabic", None),
    dg("as_IN", "অসমীয়া", "Assamese", BENGALI),
    dg("bn_BD", "বাংলা (বাংলাদেশ)", "Bengali (Bangladesh)", BENGALI),
    dg("bn_IN", "বাংলা (ভারত)", "Bengali (India)", BENGALI),
    l("brx_IN", "बड़ो", "Bodo"),
    l("my_MM", "မြန်မာ", "Burmese").with_digits("၀၁၂၃၄၅၆၇၈၉"),
    l("ca_ES", "Català", "Catalan"),
    l("zh_CN", "简体中文", "Chinese (Simplified)"),
    l("zh_TW", "繁體中文", "Chinese (Traditional)"),
    l("cs_CZ", "Čeština", "Czech"),
    l("da_DK", "Dansk", "Danish"),
    l("doi_IN", "डोगरी", "Dogri"),
    l("nl_NL", "Nederlands", "Dutch"),
    dg("dz_BT", "རྫོང་ཁ", "Dzongkha", "༠༡༢༣༤༥༦༧༨༩"),
    l("fil_PH", "Filipino", "Filipino"),
    l("nl_BE", "Nederlands (België)", "Flemish"),
    l("fr_FR", "Français", "French"),
    l("ka_GE", "ქართული", "Georgian"),
    l("de_DE", "Deutsch", "German"),
    l("el_GR", "Ελληνικά", "Greek"),
    l("gu_IN", "ગુજરાતી", "Gujarati"),
    rtl("he_IL", "עברית", "Hebrew", None),
    l("hi_IN", "हिन्दी", "Hindi"),
    l("hu_HU", "Magyar", "Hungarian"),
    l("id_ID", "Bahasa Indonesia", "Indonesian"),
    l("it_IT", "Italiano", "Italian"),
    l("ja_JP", "日本語", "Japanese"),
    l("kn_IN", "ಕನ್ನಡ", "Kannada"),
    rtl("ks_IN", "کٲشُر", "Kashmiri", Some(PERSIAN)),
    l("km_KH", "ខ្មែរ", "Khmer"),
    l("kok_IN", "कोंकणी", "Konkani"),
    l("ko_KR", "한국어", "Korean"),
    l("lv_LV", "Latviešu", "Latvian"),
    l("lt_LT", "Lietuvių", "Lithuanian"),
    l("mai_IN", "मैथिली", "Maithili"),
    l("ms_MY", "Bahasa Melayu", "Malay"),
    l("ml_IN", "മലയാളം", "Malayalam"),
    dg("mni_IN", "ꯃꯤꯇꯩꯂꯣꯟ", "Manipuri", BENGALI),
    dg("mr_IN", "मराठी", "Marathi", DEVANAGARI),
    dg("ne_IN", "नेपाली", "Nepali", DEVANAGARI),
    l("no_NO", "Norsk", "Norwegian"),
    l("or_IN", "ଓଡ଼ିଆ", "Odia"),
    rtl("fa_IR", "فارسی", "Persian", Some(PERSIAN)),
    l("pl_PL", "Polski", "Polish"),
    l("pt_BR", "Português (Brasil)", "Portuguese (Brazil)"),
    l("pa_IN", "ਪੰਜਾਬੀ", "Punjabi"),
    l("ro_RO", "Română", "Romanian"),
    l("ru_RU", "Русский", "Russian"),
    dg("sa_IN", "संस्कृतम्", "Sanskrit", DEVANAGARI),
    dg("sat_IN", "ᱥᱟᱱᱛᱟᱲᱤ", "Santali", "᱐᱑᱒᱓᱔᱕᱖᱗᱘᱙"),
    rtl("sd_IN", "سنڌي", "Sindhi", None),
    l("si_LK", "සිංහල", "Sinhala"),
    l("sl_SI", "Slovenščina", "Slovenian"),
    l("es_ES", "Español", "Spanish"),
    l("sw_KE", "Kiswahili", "Swahili"),
    l("sv_SE", "Svenska", "Swedish"),
    l("ta_IN", "தமிழ்", "Tamil"),
    l("te_IN", "తెలుగు", "Telugu"),
    l("th_TH", "ไทย", "Thai"),
    l("tr_TR", "Türkçe", "Turkish"),
    l("uk_UA", "Українська", "Ukrainian"),
    rtl("ur_IN", "اردو", "Urdu", Some(PERSIAN)),
    l("vi_VN", "Tiếng Việt", "Vietnamese"),
];

impl Language {
    const fn with_digits(mut self, d: &'static str) -> Self {
        self.digits = Some(d);
        self
    }
}

/// The language by code; `hi`, `hi-IN` and `hi_IN` all find Hindi.
pub fn find(code: &str) -> Option<&'static Language> {
    let c = code.replace('-', "_");
    LANGUAGES.iter().find(|l| l.code.eq_ignore_ascii_case(&c)).or_else(|| {
        let base = c.split('_').next().unwrap_or("");
        LANGUAGES.iter().find(|l| l.code.split('_').next().is_some_and(|b| b.eq_ignore_ascii_case(base)))
    })
}

struct Current {
    lang: &'static Language,
    texts: &'static HashMap<&'static str, &'static str>,
}

static CURRENT: RwLock<Option<Current>> = RwLock::new(None);

/// The language shown now.
pub fn current() -> &'static Language {
    CURRENT.read().ok().and_then(|c| c.as_ref().map(|c| c.lang)).unwrap_or(&LANGUAGES[0])
}

/// The chosen language for month and day names in dates (`%b`, `%a`).
pub fn date_locale() -> chrono::Locale {
    let code = match current().code {
        "ne_IN" => "ne_NP",
        "no_NO" => "nb_NO",
        c => c,
    };
    chrono::Locale::try_from(code).unwrap_or(chrono::Locale::POSIX)
}

/// A date or time with month and day names in the chosen language.
pub fn date<Tz: chrono::TimeZone>(d: &chrono::DateTime<Tz>, fmt: &str) -> String
where
    Tz::Offset: std::fmt::Display,
{
    d.format_localized(fmt, date_locale()).to_string()
}

/// Shown right to left now.
pub fn is_rtl() -> bool {
    current().rtl
}

/// Switches the language for every text read from now on. Each language's
/// texts are read once and kept for the run.
pub fn set_language(code: &str) {
    let lang = find(code).unwrap_or(&LANGUAGES[0]);
    let texts = load(lang.code);
    if let Ok(mut c) = CURRENT.write() {
        *c = Some(Current { lang, texts });
    }
}

/// The language to start in: the saved choice, else the system's, else English.
pub fn startup_code(saved: Option<&str>) -> &'static str {
    if let Some(l) = saved.and_then(find) {
        return l.code;
    }
    system_locale().as_deref().and_then(find).map(|l| l.code).unwrap_or("en_US")
}

fn system_locale() -> Option<String> {
    #[cfg(windows)]
    {
        windows_locale()
    }
    #[cfg(not(windows))]
    {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
            .map(|v| v.split('.').next().unwrap_or("").to_string())
    }
}

#[cfg(windows)]
fn windows_locale() -> Option<String> {
    // GetUserDefaultLocaleName: "hi-IN".
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultLocaleName(name: *mut u16, len: i32) -> i32;
    }
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..(n - 1) as usize]))
}

/// The texts of a language, read once from the copy built into the program.
fn load(code: &str) -> &'static HashMap<&'static str, &'static str> {
    static CACHE: RwLock<Vec<(&'static str, &'static HashMap<&'static str, &'static str>)>> = RwLock::new(Vec::new());
    if let Some(m) = CACHE.read().ok().and_then(|c| c.iter().find(|(c2, _)| *c2 == code).map(|(_, m)| *m)) {
        return m;
    }
    let mut map: HashMap<&'static str, &'static str> = HashMap::new();
    if let Some(json) = bundled(code) {
        match serde_json::from_str::<HashMap<String, String>>(&json) {
            Ok(m) => {
                for (k, v) in m {
                    if !v.is_empty() {
                        map.insert(Box::leak(k.into_boxed_str()), Box::leak(v.into_boxed_str()));
                    }
                }
            }
            Err(e) => eprintln!("i18n: {code}: {e}"),
        }
    }
    let leaked: &'static HashMap<&'static str, &'static str> = Box::leak(Box::new(map));
    let code: &'static str = find(code).map(|l| l.code).unwrap_or("en_US");
    if let Ok(mut c) = CACHE.write() {
        c.push((code, leaked));
    }
    leaked
}

#[cfg(feature = "gui")]
mod bundle {
    include!(concat!(env!("OUT_DIR"), "/i18n_bundle.rs"));
}

/// A language's JSON, from the compressed copy in the program.
#[cfg(feature = "gui")]
fn bundled(code: &str) -> Option<String> {
    let (_, bytes) = bundle::LOCALES.iter().find(|(c, _)| *c == code)?;
    let mut out = Vec::new();
    let mut r = flate2::read::DeflateDecoder::new(*bytes);
    std::io::Read::read_to_end(&mut r, &mut out).ok()?;
    String::from_utf8(out).ok()
}

/// The server agent shows nothing on screen: English only.
#[cfg(not(feature = "gui"))]
fn bundled(_code: &str) -> Option<String> {
    None
}

fn texts() -> Option<&'static HashMap<&'static str, &'static str>> {
    CURRENT.read().ok().and_then(|c| c.as_ref().map(|c| c.texts))
}

/// `t!`: the text in the language shown now.
pub fn t(key: &'static str) -> &'static str {
    texts().and_then(|m| m.get(key).copied()).unwrap_or(key)
}

/// A text that came from elsewhere (a job's steps and messages, the server
/// agent): its translation when it is one of the catalogue's texts, as is or
/// with its placeholders filled ("Copying 12 GB to D:\\Data" from
/// "Copying {0} to {1}"). The job's own words stay English inside, since the
/// plan and the work match steps by name.
pub fn dynamic(text: &str) -> std::borrow::Cow<'_, str> {
    let Some(m) = texts() else { return text.into() };
    if let Some(v) = m.get(text) {
        return (*v).into();
    }
    if !text.chars().any(|c| c.is_alphabetic()) {
        return text.into();
    }
    let code = current().code;
    if let Ok(cache) = DYNAMIC.read() {
        if let Some((c, map)) = cache.as_ref() {
            if *c == code {
                if let Some(v) = map.get(text) {
                    return v.clone().map(Into::into).unwrap_or_else(|| text.into());
                }
            }
        }
    }
    let found = templates(code, m).iter().find_map(|tp| tp.fill(text, current().digits)).or_else(|| list(text));
    if let Ok(mut cache) = DYNAMIC.write() {
        if cache.as_ref().is_none_or(|(c, map)| *c != code || map.len() > 4000) {
            *cache = Some((code, HashMap::new()));
        }
        if let Some((_, map)) = cache.as_mut() {
            map.insert(text.to_string(), found.clone());
        }
    }
    found.map(Into::into).unwrap_or_else(|| text.into())
}

/// Whether a label shown in the current language is one of the English
/// texts that start with `english` ("Backing up", "Updating SENTIENT on {0}").
/// A translated label keeps the meaning but not the word order (the verb comes
/// last in Tamil or Japanese), so it is matched against those texts'
/// translations, not by its first word.
pub fn label_starts(label: &str, english: &str) -> bool {
    if label.starts_with(english) {
        return true;
    }
    let Some(m) = texts() else { return false };
    m.iter().filter(|(k, _)| k.starts_with(english)).any(|(k, v)| {
        if k.contains('{') {
            Template::parse(v, k).is_some_and(|tp| tp.fill(label, None).is_some())
        } else {
            label.starts_with(*v)
        }
    })
}

/// A list joined by ", " ("Compressed, split over 4 GB, encrypted"): each
/// part that is a catalogue text is translated; None when none is.
fn list(text: &str) -> Option<String> {
    if !text.contains(", ") {
        return None;
    }
    let parts: Vec<std::borrow::Cow<'_, str>> = text.split(", ").map(dynamic).collect();
    text.split(", ").zip(&parts).any(|(a, b)| a != b.as_ref()).then(|| parts.join(", "))
}

type DynCache = Option<(&'static str, HashMap<String, Option<String>>)>;
static DYNAMIC: RwLock<DynCache> = RwLock::new(None);

/// A catalogue text with placeholders, as pieces to match a filled-in text.
struct Template {
    /// The English between the placeholders: n + 1 pieces for n placeholders.
    pieces: Vec<&'static str>,
    names: Vec<&'static str>,
    translation: &'static str,
}

impl Template {
    fn parse(key: &'static str, translation: &'static str) -> Option<Template> {
        let mut pieces = Vec::new();
        let mut names = Vec::new();
        let mut rest = key;
        while let Some(open) = rest.find('{') {
            if rest[open..].starts_with("{{") {
                return None; // braces as text: not worth matching
            }
            let close = rest[open..].find('}')? + open;
            pieces.push(&rest[..open]);
            names.push(&rest[open + 1..close]);
            rest = &rest[close + 1..];
        }
        pieces.push(rest);
        // a template with two placeholders side by side cannot be told apart
        if names.is_empty() || pieces[1..pieces.len() - 1].iter().any(|p| p.is_empty()) {
            return None;
        }
        Some(Template { pieces, names, translation })
    }

    /// The English words it fixes: longer is more specific.
    fn weight(&self) -> usize {
        self.pieces.iter().map(|p| p.len()).sum()
    }

    fn fill(&self, text: &str, digits: Option<&str>) -> Option<String> {
        let mut rest = text.strip_prefix(self.pieces[0])?;
        let mut values = Vec::with_capacity(self.names.len());
        for (i, piece) in self.pieces[1..].iter().enumerate() {
            let last = i == self.pieces.len() - 2;
            let at = if last {
                if !rest.ends_with(piece) {
                    return None;
                }
                rest.len() - piece.len()
            } else {
                rest.find(piece)?
            };
            values.push((self.names[i], rest[..at].to_string()));
            rest = &rest[at + piece.len()..];
        }
        Some(fill(self.translation, &values, digits))
    }
}

/// The language's templates, most specific first, made once per language.
fn templates(code: &'static str, m: &'static HashMap<&'static str, &'static str>) -> &'static [Template] {
    static MADE: RwLock<Vec<(&'static str, &'static [Template])>> = RwLock::new(Vec::new());
    if let Some(t) = MADE.read().ok().and_then(|v| v.iter().find(|(c, _)| *c == code).map(|(_, t)| *t)) {
        return t;
    }
    let mut list: Vec<Template> = m.iter().filter(|(k, _)| k.contains('{')).filter_map(|(k, v)| Template::parse(k, v)).collect();
    list.sort_by(|a, b| b.weight().cmp(&a.weight()));
    let leaked: &'static [Template] = Box::leak(list.into_boxed_slice());
    if let Ok(mut v) = MADE.write() {
        v.push((code, leaked));
    }
    leaked
}

/// `tf!`: the text in the language shown now, its placeholders filled.
pub fn tf(key: &'static str, args: &[(&str, String)]) -> String {
    let template = t(key);
    let digits = current().digits;
    fill(template, args, digits)
}

/// A value written with the language's digits when it is a plain number
/// ("412", "64.1", "1,024", "-3"); names, versions and addresses keep theirs.
fn localize_number<'a>(v: &'a str, digits: Option<&str>) -> std::borrow::Cow<'a, str> {
    let Some(d) = digits else { return v.into() };
    let plain = !v.is_empty()
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | ' ' | '\u{202f}'))
        && v.matches('.').count() <= 1;
    if !plain {
        return v.into();
    }
    let table: Vec<char> = d.chars().collect();
    v.chars().map(|c| if c.is_ascii_digit() { table[(c as u8 - b'0') as usize] } else { c }).collect::<String>().into()
}

fn fill(template: &str, args: &[(&str, String)], digits: Option<&str>) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut it = template.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' if it.peek() == Some(&'{') => {
                it.next();
                out.push('{');
            }
            '}' if it.peek() == Some(&'}') => {
                it.next();
                out.push('}');
            }
            '{' => {
                let mut name = String::new();
                for c in it.by_ref() {
                    if c == '}' {
                        break;
                    }
                    name.push(c);
                }
                match args.iter().find(|(n, _)| *n == name.trim()) {
                    Some((_, v)) => out.push_str(&localize_number(v, digits)),
                    // a translation's broken placeholder: shown as written
                    None => {
                        out.push('{');
                        out.push_str(&name);
                        out.push('}');
                    }
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_named_and_positional() {
        let a = [("0", "3".to_string()), ("name", "db".to_string())];
        assert_eq!(fill("{0} of {name} {{x}}", &a, None), "3 of db {x}");
        assert_eq!(fill("{name} का {0}", &a, Some(DEVANAGARI)), "db का ३");
    }

    #[test]
    fn versions_keep_their_digits() {
        assert_eq!(localize_number("4.4.0.3", Some(BENGALI)), "4.4.0.3");
        assert_eq!(localize_number("10.0.0.1", Some(BENGALI)), "10.0.0.1");
        assert_eq!(localize_number("64.1", Some(BENGALI)), "৬৪.১");
        assert_eq!(localize_number("PLC-2", Some(BENGALI)), "PLC-2");
    }

    #[test]
    fn matches_filled_texts() {
        let t = Template::parse("Copying {0} to {to}", "{to} में {0} कॉपी हो रहा है").unwrap();
        assert_eq!(t.fill("Copying 12 GB to D:\\Data", None).as_deref(), Some("D:\\Data में 12 GB कॉपी हो रहा है"));
        assert!(t.fill("Moving 12 GB to D:", None).is_none());
        assert!(Template::parse("{0}{1}", "x").is_none());
    }

    #[test]
    fn finds_languages() {
        assert_eq!(find("hi-IN").unwrap().code, "hi_IN");
        assert_eq!(find("de").unwrap().code, "de_DE");
        assert_eq!(LANGUAGES.len(), 64);
    }
}
