// Image preview support — renders inline image previews in the terminal.
//
// TODO: Port from kokoirc's image preview system. Will support:
// - URL detection in messages (imgur, tenor, direct image links)
// - Async image fetching with timeout and size limits
// - Terminal protocol detection (kitty, sixel, iterm2, chafa fallback)
// - Image caching with configurable size/age limits
// - Render via ratatui-image crate
