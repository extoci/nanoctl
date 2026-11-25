/// Byte order of captured RGBA-style pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PixelFormat {
    #[default]
    Rgba,
    Bgra,
}
