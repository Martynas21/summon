/// Windows has no Accessibility permission gate equivalent to macOS TCC.
pub fn is_trusted() -> bool {
    true
}

/// No-op on Windows; always returns true.
pub fn request_trust() -> bool {
    true
}
