//! Adds the program name, description and version to the Windows file properties.

fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "TL Supporter");
    res.set("FileDescription", "TL Supporter - LAN assistance tool for team leads");
    res.set("CompanyName", "SemkoArtem");
    res.set("LegalCopyright", "Copyright (c) 2026 SemkoArtem");
    res.set("OriginalFilename", "TLSupporter.exe");
    res.compile().expect("failed to embed Windows file properties");
}
