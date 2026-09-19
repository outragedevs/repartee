use ratatui::layout::Rect;
use std::io::Write;

const CHARS_PER_CHUNK: usize = 4096;
const CHUNK_SIZE: usize = (CHARS_PER_CHUNK / 4) * 3;
const MOUSE_DISABLE: &[u8] = b"\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l";
const MOUSE_ENABLE: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";

pub fn write_kitty(out: &mut impl Write, raw_png: &[u8], rect: Rect) {
    let Rect {
        x: inner_x,
        y: inner_y,
        width: inner_w,
        height: inner_h,
    } = rect;

    let row = inner_y + 1;
    let col = inner_x + 1;
    let _ = write!(out, "\x1b7\x1b[{row};{col}H");
    let _ = out.flush();

    let chunks: Vec<&[u8]> = raw_png.chunks(CHUNK_SIZE).collect();
    let chunk_count = chunks.len();

    for (i, chunk) in chunks.iter().enumerate() {
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, chunk);
        let more = u8::from(i + 1 < chunk_count);

        if i == 0 {
            let _ = write!(
                out,
                "\x1bPtmux;\x1b\x1b_Gq=2,a=T,f=100,t=d,c={inner_w},r={inner_h},m={more};{b64}\x1b\x1b\\\x1b\\"
            );
        } else {
            let _ = write!(out, "\x1bPtmux;\x1b\x1b_Gm={more};{b64}\x1b\x1b\\\x1b\\");
        }
        let _ = out.flush();
    }

    let _ = write!(out, "\x1b8");
    let _ = out.flush();
}

pub fn write_iterm2(out: &mut impl Write, raw_png: &[u8], rect: Rect) {
    let Rect {
        x: inner_x,
        y: inner_y,
        width: inner_w,
        height: inner_h,
    } = rect;

    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw_png);

    let osc = format!(
        "\x1b]1337;File=inline=1;width={inner_w};height={inner_h};preserveAspectRatio=0:{b64}\x07"
    );

    let escaped = osc.replace('\x1b', "\x1b\x1b");
    let dcs = format!("\x1bPtmux;{escaped}\x1b\\");

    let row = inner_y + 1;
    let col = inner_x + 1;

    let _ = out.write_all(MOUSE_DISABLE);
    let _ = out.flush();

    let _ = write!(out, "\x1b7\x1b[{row};{col}H");
    let _ = out.flush();

    let _ = out.write_all(dcs.as_bytes());
    let _ = out.flush();

    let _ = out.write_all(b"\x1b8");
    let _ = out.flush();

    let _ = out.write_all(MOUSE_ENABLE);
    let _ = out.flush();
}
