/// Byte order of captured RGBA-style pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PixelFormat {
    #[default]
    Rgba,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    Bgra,
}
