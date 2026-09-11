use std::collections::HashMap;

struct CaptureFrame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fn capture_screen(width: u32, height: u32) -> CaptureFrame {
    CaptureFrame {
        width,
        height,
        pixels: vec![0u8; (width * height * 4) as usize],
    }
}

fn main() {
    let mut frames: Vec<CaptureFrame> = Vec::new();
    frames.push(capture_screen(1920, 1080));

    // hold a reference to the first frame's pixel buffer
    let first_buffer = &frames[0].pixels;

    // then push another frame -- this reallocates the Vec
    frames.push(capture_screen(1920, 1080));

    println!("first frame has {} bytes", first_buffer.len());

    let mut cache: HashMap<String, u32> = HashMap::new();
    cache.insert("frames".to_string(), frames.len() as u32);
    println!("{:?}", cache);
}
