use anyhow::Result;

const GOOGLE_NEWS_RSS: &str = "https://news.google.com/rss/search";

/// Search Google News RSS for a query and return formatted headlines.
/// Used as a client-side web search tool for the Gemini agent.
pub async fn web_search(query: &str) -> String {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    match fetch_google_news(&client, query).await {
        Ok(headlines) if !headlines.is_empty() => headlines,
        Ok(_) => "No relevant news found.".to_string(),
        Err(e) => format!("Search failed: {e}"),
    }
}

/// Fetch recent news headlines from Google News RSS for a query.
async fn fetch_google_news(client: &reqwest::Client, query: &str) -> Result<String> {
    let url = format!(
        "{GOOGLE_NEWS_RSS}?q={}&hl=en-US&gl=US&ceid=US:en",
        urlencode(query)
    );

    let resp = client.get(&url).send().await?;
    let body = resp.text().await?;
    let headlines = extract_rss_titles(&body);

    if headlines.is_empty() {
        return Ok(String::new());
    }

    // Format as numbered list of recent headlines
    let summary = headlines
        .into_iter()
        .take(4)
        .enumerate()
        .map(|(i, h)| format!("{}. {}", i + 1, h))
        .collect::<Vec<_>>()
        .join("; ");

    Ok(summary)
}

/// Extract titles from RSS XML `<item>` elements.
fn extract_rss_titles(xml: &str) -> Vec<String> {
    let mut titles = Vec::new();

    // Find each <item>...</item> block, then extract <title>...</title> within it
    let mut rest = xml;
    while let Some(item_start) = rest.find("<item>") {
        if titles.len() >= 6 {
            break;
        }
        rest = &rest[item_start + 6..];
        let item_end = rest.find("</item>").unwrap_or(rest.len());
        let item_body = &rest[..item_end];

        if let Some(title_start) = item_body.find("<title>") {
            let after_tag = &item_body[title_start + 7..];
            if let Some(title_end) = after_tag.find("</title>") {
                let title = decode_xml_entities(&after_tag[..title_end])
                    .trim()
                    .to_string();
                if !title.is_empty() {
                    titles.push(title);
                }
            }
        }

        rest = &rest[item_end..];
    }

    titles
}

/// Simple URL encoding for query parameters.
fn urlenccode_char(c: char, out: &mut String) {
    match c {
        'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
        ' ' => out.push('+'),
        _ => {
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            for byte in encoded.bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
}

pub fn urlencode(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        urlenccode_char(c, &mut result);
    }
    result
}

/// Decode common XML/HTML entities.
fn decode_xml_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_xml_entities() {
        assert_eq!(decode_xml_entities("a &amp; b"), "a & b");
        assert_eq!(decode_xml_entities("&lt;tag&gt;"), "<tag>");
    }

    #[test]
    fn test_urlenccode() {
        assert_eq!(urlencode("hello world"), "hello+world");
        assert_eq!(urlencode("a&b"), "a%26b");
    }

    #[test]
    fn test_extract_rss_titles() {
        let xml = r#"<?xml version="1.0"?>
<rss><channel>
<title>Test Feed</title>
<item><title>Player X signs big deal with Team Y - ESPN</title><link>http://example.com</link></item>
<item><title>Player X injury update: expected back Week 10 - NFL.com</title><link>http://example.com</link></item>
</channel></rss>"#;
        let titles = extract_rss_titles(xml);
        assert_eq!(titles.len(), 2);
        assert!(titles[0].contains("signs big deal"));
        assert!(titles[1].contains("injury update"));
    }
}
