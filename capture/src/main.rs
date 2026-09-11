use std::sync::mpsc::{channel, Sender};

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

/// One captured frame, as owned data. No borrows, so it can leave the callback.
struct Shot {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

/// The capture handler. It owns the sending half of a channel and posts one
/// `Shot` back to `main` before stopping the capture.
struct Capture {
    sender: Sender<Shot>,
}

impl GraphicsCaptureApiHandler for Capture {
    /// Whatever we pass into `Settings::new` arrives here. We use it to hand
    /// the handler its channel, because the framework constructs it for us.
    type Flags = Sender<Shot>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { sender: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let width = frame.width();
        let height = frame.height();

        let buffer = frame.buffer()?;
        let mut scratch = Vec::new();

        // `as_nopadding_buffer` returns a *view* into GPU memory that is unmapped
        // when this callback returns, so `.to_vec()` copies it into memory we own.
        // That copy is what lets the data outlive the frame.
        let pixels = buffer.as_nopadding_buffer(&mut scratch).to_vec();

        self.sender.send(Shot {
            pixels,
            width,
            height,
        })?;

        // One frame is all we wanted.
        control.stop();
        Ok(())
    }
}

fn main() {
    // Optional first argument: seconds to wait before capturing, so you have
    // time to bring something else to the foreground.
    let delay_secs: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(0);

    if delay_secs > 0 {
        println!("capturing in {delay_secs}s...");
        std::thread::sleep(std::time::Duration::from_secs(delay_secs));
    }

    let monitor = Monitor::primary().expect("no primary monitor found");
    let (sender, receiver) = channel::<Shot>();

    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        sender,
    );

    // Blocking: runs a Windows message loop until `control.stop()` fires.
    Capture::start(settings).expect("screen capture failed");

    let shot = receiver.recv().expect("capture produced no frame");
    println!(
        "captured {}x{} ({} bytes)",
        shot.width,
        shot.height,
        shot.pixels.len()
    );

    image::save_buffer(
        "out.png",
        &shot.pixels,
        shot.width,
        shot.height,
        image::ExtendedColorType::Rgba8,
    )
    .expect("failed to write out.png");

    println!("wrote out.png");
}
