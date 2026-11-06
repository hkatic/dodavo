//! Android's string resources (`res/values*/strings.xml`), made from the table
//! of the Android interface (`dodavo_core::i18n::android`), so that a key or a
//! translation that is missing fails to compile there as it does here. A test
//! checks that the committed files are current; write them anew with
//!
//!     cargo run -p dodavo-android --example gen_strings -- android/app/src/main/res
//!
//! A text's `{name}` placeholders become positional arguments (`%1$s`, ...),
//! numbered in the order in which they first come in the English text, the
//! same in every language. In a plural, `{n}` is always `%1$d`, and the others
//! follow from `%2$s`. Each value is quoted, so that its spaces stay as they are.

use dodavo_core::i18n::{Category, Lang, Plural, android};

/// The resource folder of each language. Serbian is there in Latin only; a
/// phone set to Cyrillic Serbian gets English, as Dodavo's own texts do then.
pub const FOLDERS: [(&str, Lang); 4] = [
    ("values", Lang::En),
    ("values-hr", Lang::Hr),
    ("values-b+sr+Latn", Lang::SrLatn),
    ("values-sl", Lang::Sl),
];

/// The names of the placeholders of `text`, in the order they first come.
fn placeholders(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        let name = &after[..end];
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
        rest = &after[end + 1..];
    }
    names
}

/// `text` as a quoted resource value, with its placeholders as the arguments
/// that `order` gives them (`(name, spec)`), escaped for aapt and XML.
fn value(text: &str, order: &[(String, String)]) -> String {
    let has_arguments = !order.is_empty();
    let mut out = String::from("\"");
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('{')
            && let Some(end) = after.find('}')
            && let Some((_, spec)) = order.iter().find(|(name, _)| *name == after[..end])
        {
            out.push_str(spec);
            rest = &after[end + 1..];
            continue;
        }
        let c = rest.chars().next().unwrap_or_default();
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '%' if has_arguments => out.push_str("%%"),
            c => out.push(c),
        }
        rest = &rest[c.len_utf8()..];
    }
    out.push('"');
    out
}

/// The positional arguments of a text, from its English form.
fn text_order(english: &str) -> Vec<(String, String)> {
    placeholders(english)
        .into_iter()
        .enumerate()
        .map(|(i, name)| (name, format!("%{}$s", i + 1)))
        .collect()
}

/// The positional arguments of a plural, from its English `other` form.
fn plural_order(english: &str) -> Vec<(String, String)> {
    let mut order = vec![("n".to_string(), "%1$d".to_string())];
    let others = placeholders(english).into_iter().filter(|name| name != "n");
    order.extend(
        others
            .enumerate()
            .map(|(i, name)| (name, format!("%{}$s", i + 2))),
    );
    order
}

/// The plural categories that a language has (CLDR).
fn categories(lang: Lang) -> &'static [Category] {
    use Category::*;
    match lang {
        Lang::En => &[One, Other],
        Lang::Hr | Lang::SrLatn => &[One, Few, Other],
        Lang::Sl => &[One, Two, Few, Other],
    }
}

fn form(plural: &Plural, category: Category) -> (&'static str, &'static str) {
    match category {
        Category::One => ("one", plural.one),
        Category::Two => ("two", plural.two),
        Category::Few => ("few", plural.few),
        Category::Other => ("other", plural.other),
    }
}

/// The resource file of `lang`.
pub fn strings_xml(lang: Lang) -> String {
    let table = android::strings(lang);
    let english = android::strings(Lang::En);
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <!-- Made from src/i18n/android by the example gen_strings of\n     \
         crates/dodavo-android. Do not edit. -->\n\
         <resources>\n",
    );
    out.push_str(&format!(
        "    <!-- The language of these texts, which the app gives Dodavo's core. -->\n    \
         <string name=\"dodavo_lang\" translatable=\"false\">{}</string>\n",
        lang.code()
    ));
    if lang == Lang::En {
        out.push_str("    <string name=\"app_name\" translatable=\"false\">Dodavo</string>\n");
    }
    for ((key, text), (_, english_text)) in table.texts().into_iter().zip(english.texts()) {
        let order = text_order(english_text);
        let formatted = if order.is_empty() && text.contains('%') {
            " formatted=\"false\""
        } else {
            ""
        };
        out.push_str(&format!(
            "    <string name=\"{key}\"{formatted}>{}</string>\n",
            value(text, &order)
        ));
    }
    for ((key, plural), (_, english_plural)) in table.plurals().into_iter().zip(english.plurals()) {
        let order = plural_order(english_plural.other);
        out.push_str(&format!("    <plurals name=\"{key}\">\n"));
        for &category in categories(lang) {
            let (quantity, text) = form(plural, category);
            out.push_str(&format!(
                "        <item quantity=\"{quantity}\">{}</item>\n",
                value(text, &order)
            ));
        }
        out.push_str("    </plurals>\n");
    }
    out.push_str("</resources>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_become_positional_arguments() {
        let order = text_order("{name} sends {size} to {name}");
        assert_eq!(
            value("{size}: {name} – 50%", &order),
            "\"%2$s: %1$s – 50%%\""
        );
        assert_eq!(value("\"A\" & <b>\nc\\", &[]), "\"\\\"A\\\" &amp; &lt;b&gt;\\nc\\\\\"");
        let order = plural_order("“{name}”  ({n} files, {size})");
        assert_eq!(
            value("„{name}”  ({n} datoteke, {size})", &order),
            "\"„%2$s”  (%1$d datoteke, %3$s)\""
        );
    }

    /// Every key is a name that Kotlin can use, and none is a keyword.
    #[test]
    fn keys_are_resource_names() {
        let table = android::strings(Lang::En);
        let keys = table
            .texts()
            .into_iter()
            .map(|(key, _)| key)
            .chain(table.plurals().into_iter().map(|(key, _)| key));
        for key in keys {
            assert!(
                key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{key}"
            );
            assert!(
                !["as", "in", "is", "object", "package", "fun", "val", "var", "when"].contains(&key),
                "{key}"
            );
        }
    }

    #[test]
    fn the_android_strings_are_current() {
        let committed = [
            include_str!("../../../android/app/src/main/res/values/strings.xml"),
            include_str!("../../../android/app/src/main/res/values-hr/strings.xml"),
            include_str!("../../../android/app/src/main/res/values-b+sr+Latn/strings.xml"),
            include_str!("../../../android/app/src/main/res/values-sl/strings.xml"),
        ];
        for ((_, lang), file) in FOLDERS.iter().zip(committed) {
            assert_eq!(
                file,
                strings_xml(*lang),
                "{lang:?}: run cargo run -p dodavo-android --example gen_strings -- android/app/src/main/res"
            );
        }
    }
}
