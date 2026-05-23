//! Russian → Latin transliteration + diacritic stripping.
//!
//! Used to normalize both queries and project signals before fuzzy
//! comparison so that "наркотики" can match "narkotiki" / "drugs".

/// Transliterate any Cyrillic / accented characters in `s` to plain ASCII.
/// Non-Cyrillic characters are passed through unchanged.
pub fn transliterate(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            // Lowercase Cyrillic
            'а' => out.push('a'),
            'б' => out.push('b'),
            'в' => out.push('v'),
            'г' => out.push('g'),
            'д' => out.push('d'),
            'е' | 'ё' | 'э' => out.push('e'),
            'ж' => out.push_str("zh"),
            'з' => out.push('z'),
            'и' | 'й' => out.push('i'),
            'к' => out.push('k'),
            'л' => out.push('l'),
            'м' => out.push('m'),
            'н' => out.push('n'),
            'о' => out.push('o'),
            'п' => out.push('p'),
            'р' => out.push('r'),
            'с' => out.push('s'),
            'т' => out.push('t'),
            'у' => out.push('u'),
            'ф' => out.push('f'),
            'х' => out.push('h'),
            'ц' => out.push_str("ts"),
            'ч' => out.push_str("ch"),
            'ш' => out.push_str("sh"),
            'щ' => out.push_str("shch"),
            'ъ' | 'ь' => {}
            'ы' => out.push('y'),
            'ю' => out.push_str("yu"),
            'я' => out.push_str("ya"),
            // Uppercase Cyrillic — go through lowercase mapping.
            'А'..='Я' | 'Ё' => {
                let lower = ch.to_lowercase().collect::<String>();
                out.push_str(&transliterate(&lower));
            }
            // Common Latin diacritics → strip.
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => out.push('a'),
            'é' | 'è' | 'ê' | 'ë' => out.push('e'),
            'í' | 'ì' | 'î' | 'ï' => out.push('i'),
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => out.push('o'),
            'ú' | 'ù' | 'û' | 'ü' => out.push('u'),
            'ñ' => out.push('n'),
            'ç' => out.push('c'),
            'ß' => out.push_str("ss"),
            other => out.push(other),
        }
    }
    out
}

/// Lowercase + transliterate. Cheap helper used in the hot path.
pub fn normalize(s: &str) -> String {
    transliterate(&s.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_words() {
        assert_eq!(transliterate("наркотики"), "narkotiki");
        assert_eq!(transliterate("плагин"), "plagin");
        assert_eq!(transliterate("Привет, Мир!"), "privet, mir!");
    }

    #[test]
    fn special_clusters() {
        assert_eq!(transliterate("щука"), "shchuka");
        assert_eq!(transliterate("Жаба"), "zhaba");
        assert_eq!(transliterate("Юлия"), "yuliya");
    }

    #[test]
    fn diacritics_stripped() {
        assert_eq!(transliterate("café"), "cafe");
        assert_eq!(transliterate("naïve"), "naive");
        assert_eq!(transliterate("straße"), "strasse");
    }

    #[test]
    fn ascii_passthrough() {
        assert_eq!(
            transliterate("drugs-tracker-plugin"),
            "drugs-tracker-plugin"
        );
        assert_eq!(transliterate("PigIDE 2.0"), "PigIDE 2.0");
    }

    #[test]
    fn normalize_lowers() {
        assert_eq!(normalize("DrugsTrackerPlugin"), "drugstrackerplugin");
        assert_eq!(normalize("Наркотики Plugin"), "narkotiki plugin");
    }
}
