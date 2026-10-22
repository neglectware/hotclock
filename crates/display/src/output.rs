//! Where the rendered frames go. The KMS output is the real one; the trait lets the application loop run
//! against any other (a test double, a window) without knowing about DRM.

use crate::canvas::Canvas;
use crate::layout::Rect;

pub trait Output {
    /// The size of the mode in use, in pixels.
    fn size(&self) -> (i32, i32);
    /// Copy the damaged rectangles of the canvas to the screen and flip.
    fn present(&mut self, canvas: &Canvas, damage: &[Rect]) -> Result<(), String>;
    /// Is the monitor still there? Cheap: reads the kernel's cached connector state.
    fn connected(&mut self) -> bool;
    /// An alarm to show on the screen: the output is running degraded (7d).
    fn alarm(&self) -> Option<String> {
        None
    }
    /// Force the 800 x 480 fallback layout (the panel refused the mode; 7d).
    fn force_fallback_layout(&self) -> bool {
        false
    }
    /// One line for the log: the connector, the mode, what the panel's EDID reported.
    fn describe(&self) -> String;
}
