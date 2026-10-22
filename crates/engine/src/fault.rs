//! Classification of device errors (architecture 1.5). Pure, so it is tested on every host. The
//! numbers are Linux errno values, which is what ALSA reports.

pub const EINTR: i32 = 4;
pub const EIO: i32 = 5;
pub const EAGAIN: i32 = 11;
pub const ENODEV: i32 = 19;
pub const EPIPE: i32 = 32;
pub const ESTRPIPE: i32 = 86;
pub const EBADFD: i32 = 77;

/// What a failed `snd_pcm_writei` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Interrupted; simply retry the write.
    Retry,
    /// Buffer underrun (`-EPIPE`): prepare and refill.
    Xrun,
    /// System suspend (`-ESTRPIPE`): resume, then prepare.
    Suspended,
    /// The device is gone (`-ENODEV`, `-EIO`): close, retry the open.
    Lost,
    /// Anything else, including `-EBADFD` (PCM in a state that cannot write): try to prepare
    /// once, then treat as lost.
    Other,
}

pub fn classify_errno(errno: i32) -> Fault {
    match errno.abs() {
        EINTR | EAGAIN => Fault::Retry,
        EPIPE => Fault::Xrun,
        ESTRPIPE => Fault::Suspended,
        ENODEV | EIO => Fault::Lost,
        _ => Fault::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_classes() {
        assert_eq!(classify_errno(-32), Fault::Xrun);
        assert_eq!(classify_errno(32), Fault::Xrun);
        assert_eq!(classify_errno(-86), Fault::Suspended);
        assert_eq!(classify_errno(-19), Fault::Lost);
        assert_eq!(classify_errno(-5), Fault::Lost);
        assert_eq!(classify_errno(-4), Fault::Retry);
        assert_eq!(classify_errno(-11), Fault::Retry);
        assert_eq!(classify_errno(-77), Fault::Other);
        assert_eq!(classify_errno(-1), Fault::Other);
    }
}
