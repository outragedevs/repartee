use super::lang::KnownLanguage;

const DEFAULT_PROMPT: &str = include_str!("system_prompt.txt");

pub fn load_template(path: &str) -> Result<String, std::io::Error> {
    if path.trim().is_empty() {
        Ok(DEFAULT_PROMPT.to_string())
    } else {
        std::fs::read_to_string(path)
    }
}

pub fn render(template: &str, source_lang: Option<&str>, target_lang: &str) -> String {
    let source = source_lang.unwrap_or("auto");
    let german = if source_lang
        .is_none_or(|lang| KnownLanguage::from_code(lang) == Some(KnownLanguage::De))
    {
        "Jeśli źródłem jest niemiecki, czytaj ae/oe/ue jak umlauty i rozpoznawaj potoczny oraz berliński zapis ze słuchu, np. nich, nen, ned, ooch, icke, weeste, haste, bissu i keen."
    } else {
        ""
    };
    template
        .replace("{{SOURCE_LANG}}", source)
        .replace("{{TARGET_LANG}}", target_lang)
        .replace("{{GERMAN_GUIDANCE}}", german)
        .replace("{{PH_OPEN}}", "__")
        .replace("{{PH_CLOSE}}", "__")
        .replace("{{PH_EXAMPLE}}", "__N1__")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_languages_and_placeholder_style() {
        let rendered = render(DEFAULT_PROMPT, Some("de"), "pl");
        assert!(rendered.contains("de") && rendered.contains("pl") && rendered.contains("__N1__"));
    }

    #[test]
    fn includes_german_guidance_for_a_regional_language_tag() {
        let rendered = render(DEFAULT_PROMPT, Some("de_DE"), "pl");
        assert!(rendered.contains("berliński"));
    }
}
