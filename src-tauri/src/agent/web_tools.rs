use reqwest::blocking::Client;
use std::time::Duration;
use url::Url;

/// Common user agents to avoid basic anti-bot blocks
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Fetches a webpage and converts its HTML to clean Markdown.
pub fn fetch_webpage(url: &str) -> Result<String, String> {
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let parsed_url = Url::parse(url).map_err(|e| format!("Invalid URL: {}", e))?;

    let response = client
        .get(parsed_url.clone())
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8")
        .header("Accept-Language", "en-US,en;q=0.5")
        .send()
        .map_err(|e| format!("Network request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP Error: {}", response.status()));
    }

    let html = response
        .text()
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    let markdown = html2md::parse_html(&html);
    
    // Trim excessively long markdown to avoid blowing up context window (limit ~100k chars)
    let max_len = 100_000;
    if markdown.len() > max_len {
        let truncated = &markdown[..max_len];
        Ok(format!("{}...\n\n[Content truncated due to length]", truncated))
    } else {
        Ok(markdown)
    }
}

/// Searches DuckDuckGo HTML version and extracts titles, links, and snippets.
pub fn search_web_duckduckgo(query: &str) -> Result<String, String> {
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let search_url = format!("https://html.duckduckgo.com/html/?q={}", urlencoding::encode(query));

    let response = client
        .get(&search_url)
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8")
        .header("Accept-Language", "en-US,en;q=0.5")
        // Referer is sometimes needed by DDG
        .header("Referer", "https://duckduckgo.com/")
        .send()
        .map_err(|e| format!("Network request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP Error from search engine: {}", response.status()));
    }

    let html = response
        .text()
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    // Since bringing in complex scrapers for one site is heavy, we'll use select.
    // Or we can just convert to markdown. DDG HTML version converted to markdown actually looks quite okay.
    // Let's try parsing it with our html2md first.
    let full_md = html2md::parse_html(&html);
    
    // To make it cleaner for the LLM, let's extract the main result block if possible,
    // but just sending the raw text if it's less than 20k chars is totally fine for modern LLMs.
    // Let's cap it to 20k to be safe.
    let max_len = 20_000;
    if full_md.len() > max_len {
        let truncated = &full_md[..max_len];
        Ok(format!("{}...\n\n[Search results truncated]", truncated))
    } else {
        Ok(full_md)
    }
}
