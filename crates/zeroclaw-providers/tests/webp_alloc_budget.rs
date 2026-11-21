//! Measures what `image-webp` really allocates and compares it with what the
//! per-image gate (`ensure_decode_within_per_image_cap`) lets through.
//!
//! The gate reads headers; the decoder allocates. The two can only drift apart
//! if the decoder changes how it sizes its buffers, or if a decode path exists
//! that the gate does not look at. Neither shows up in a test that only checks
//! the gate's verdict, so this file counts bytes.
//!
//! It is its own test binary because a `#[global_allocator]` counts every
//! thread of the process: the lib's unit tests run in parallel and would
//! pollute the numbers. Inside this binary the tests take `SERIAL`, so only
//! one measures at a time.
//!
//! What is counted is memory *requested* from the allocator, not memory made
//! resident. The decoder asks for zeroed planes (`calloc`) and a bitstream that
//! then fails never touches most of those pages, so a request of hundreds of MiB
//! can leave the process's resident set small. The per-image cap is read here
//! as a cap on what the decoder asks for, which is what `Limits` and the gate's
//! projection also describe; this file does not measure resident memory.
//!
//! Real lossy WebPs of several sizes are committed under `tests/testdata/`
//! (Pillow 10.4.0 / libwebp, `quality=50, method=6`). They are flat colour,
//! except `lossy_still_noise_256x256.webp`, so their PNG re-encodings are
//! smaller than a photograph's: the PNG term below is a lower bound on a real
//! image's.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use zeroclaw_providers::multimodal::ensure_decode_within_per_image_cap;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(by: usize) {
    let live = LIVE.fetch_add(by, Relaxed) + by;
    PEAK.fetch_max(live, Relaxed);
}

// SAFETY: every call is forwarded unchanged to `System`; the counters only
// observe sizes and never touch the memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            if new_size >= layout.size() {
                grow(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

static SERIAL: Mutex<()> = Mutex::new(());

/// Runs `f` and returns its result with the most bytes that were requested and
/// not yet freed at once during the call, beyond what was live when it started.
/// Requested, not resident: see the module documentation.
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let out = f();
    let peak = PEAK.load(Relaxed).saturating_sub(base) as u64;
    (out, peak)
}

const MIB: u64 = 1024 * 1024;
/// Mirrors `MAX_DECODED_IMAGE_ALLOC_BYTES` in `multimodal.rs`, which is private.
const PER_IMAGE_CAP: u64 = 64 * MIB;
/// What the decoder allocates regardless of size (tables, buffers, the input
/// reader); the gate charges per pixel only, so a 1x1 file is charged 8 bytes
/// and measures about 5 KiB. It is added to the charge when comparing, so tiny
/// files do not fail on a cost the per-pixel charge never claimed to cover; it
/// is 0.1% of the cap.
const FIXED_DECODER_OVERHEAD: u64 = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Still,
    Alpha,
    Animated,
}

impl Shape {
    /// Output bytes per pixel plus the scratch the gate charges per pixel
    /// (`WEBP_STILL_SCRATCH_BYTES_PER_PIXEL` = 5, `WEBP_STILL_ALPHA_...` = 8,
    /// `WEBP_ANIMATED_...` = 17).
    /// Mirrored here because the constants are private. Nothing ties the copy
    /// to the source: if a constant is retuned, the sanity asserts and boundary
    /// cases in `multimodal.rs` follow it, but this table does not, and the
    /// differential comparisons below would then use stale figures.
    fn charged_bytes_per_pixel(self) -> u64 {
        match self {
            Shape::Still => 3 + 5,
            Shape::Alpha => 4 + 8,
            Shape::Animated => 4 + 17,
        }
    }
}

macro_rules! fixture {
    ($shape:expr, $w:expr, $h:expr, $file:expr) => {
        Fixture {
            name: $file,
            shape: $shape,
            width: $w,
            height: $h,
            bytes: include_bytes!(concat!("testdata/", $file)),
        }
    };
}

struct Fixture {
    name: &'static str,
    shape: Shape,
    width: u64,
    height: u64,
    bytes: &'static [u8],
}

impl Fixture {
    fn charged(&self) -> u64 {
        self.width * self.height * self.shape.charged_bytes_per_pixel()
    }
}

fn fixtures() -> Vec<Fixture> {
    use Shape::{Alpha, Animated, Still};
    vec![
        fixture!(Still, 1, 1, "lossy_still_1x1.webp"),
        fixture!(Still, 16, 16, "lossy_still_16x16.webp"),
        fixture!(Still, 17, 17, "lossy_still_17x17.webp"),
        fixture!(Still, 33, 17, "lossy_still_33x17.webp"),
        fixture!(Still, 64, 64, "lossy_still_64x64.webp"),
        fixture!(Still, 255, 257, "lossy_still_255x257.webp"),
        fixture!(Still, 1, 512, "lossy_still_1x512.webp"),
        fixture!(Still, 512, 512, "lossy_still_512x512.webp"),
        fixture!(Still, 2048, 2048, "lossy_still_2048x2048.webp"),
        fixture!(Still, 256, 256, "lossy_still_noise_256x256.webp"),
        fixture!(Alpha, 33, 17, "lossy_alpha_33x17.webp"),
        fixture!(Alpha, 512, 512, "lossy_alpha_512x512.webp"),
        fixture!(Alpha, 1024, 1024, "lossy_alpha_1024x1024.webp"),
        fixture!(Animated, 64, 64, "lossy_anim_64x64.webp"),
        fixture!(Animated, 1024, 1024, "lossy_anim_1024x1024.webp"),
        fixture!(Animated, 2048, 2048, "lossy_anim_2048x2048.webp"),
    ]
}

fn gate(name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    ensure_decode_within_per_image_cap(name, "image/webp", bytes)
}

/// What the validator does with a WebP (`validate_image_content_with_projection`):
/// every frame of an animation, or the single image.
fn decode_like_the_validator(shape: Shape, bytes: &[u8]) -> Result<(), image::ImageError> {
    let decoder = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(bytes))?;
    if shape == Shape::Animated {
        for frame in image::AnimationDecoder::into_frames(decoder) {
            black_box(frame?);
        }
    } else {
        black_box(image::DynamicImage::from_decoder(decoder)?);
    }
    Ok(())
}

/// What `webp_to_png` does after its gate (`media_pipeline.rs`): decode with
/// `load_from_memory_with_format` and encode PNG. Repeated here because that
/// function is private to the channels crate.
fn ingest_like_webp_to_png(bytes: &[u8]) -> Result<usize, image::ImageError> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::WebP)?;
    let mut cursor = std::io::Cursor::new(Vec::new());
    image.write_to(&mut cursor, image::ImageFormat::Png)?;
    Ok(cursor.into_inner().len())
}

/// Offset of the width field in the header of the first `VP8 ` bitstream in
/// `webp`: chunk header (8) + frame tag (3) + start code `9d 01 2a` (3).
fn vp8_header_at(webp: &[u8]) -> usize {
    webp.windows(4)
        .enumerate()
        .find(|(i, window)| {
            *window == b"VP8 " && webp.get(i + 11..i + 14) == Some(&[0x9d, 0x01, 0x2a])
        })
        .map(|(i, _)| i + 14)
        .expect("a VP8 chunk with a start code")
}

/// Sets the dimensions in the header of the first `VP8 ` bitstream in `webp`
/// (two little-endian 14-bit sizes after the start code).
fn with_vp8_header_of(webp: &[u8], side: u16) -> Vec<u8> {
    let at = vp8_header_at(webp);
    let mut out = webp.to_vec();
    out[at..at + 2].copy_from_slice(&side.to_le_bytes());
    out[at + 2..at + 4].copy_from_slice(&side.to_le_bytes());
    out
}

#[test]
fn the_counter_sees_a_known_allocation_and_a_quiet_call() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (_, big) = peak_of(|| black_box(vec![1u8; 10 * MIB as usize]).len());
    assert!(
        (10 * MIB..11 * MIB).contains(&big),
        "10 MiB vec measured as {big}"
    );
    let (_, none) = peak_of(|| black_box(2 + 2));
    assert!(none < 1024, "a call that allocates nothing measured {none}");
}

/// The premise behind the gate: `image-webp` sizes its planes from the VP8
/// bitstream header, not from the canvas, before it can notice the two differ.
/// If this fails, either the decoder now compares first (and the check in
/// `projected_allocation` guards a risk that no longer exists), or the counter
/// is broken. Predictions written before the first run: unmodified header,
/// a few KiB; 8192 side, about 96 MiB; 16383 side, about 384 MiB *requested*.
/// The file has no pixel data after the header, so the decode fails before the
/// zeroed planes are touched: these are requested bytes, not resident ones.
#[test]
fn canary_the_decoder_allocates_from_the_bitstream_header_not_the_canvas() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let alpha = include_bytes!("testdata/lossy_alpha_33x17.webp");

    let (decoded, control) = peak_of(|| ingest_like_webp_to_png(alpha));
    decoded.expect("the real file decodes");
    assert!(control < MIB, "unmodified 33x17 peaked at {control} bytes");

    for (side, at_least) in [(8192u16, 90 * MIB), (16383, 360 * MIB)] {
        let hostile = with_vp8_header_of(alpha, side);
        let (result, peak) = peak_of(|| ingest_like_webp_to_png(&hostile));
        assert!(result.is_err(), "no pixel data follows the header");
        assert!(
            peak >= at_least,
            "a {side}x{side} VP8 header inside a 33x17 canvas peaked at {} MiB, expected at least {} MiB",
            peak / MIB,
            at_least / MIB
        );
        // The same bytes must not get past the gate that exists for this.
        assert!(
            gate("hostile.webp", &hostile).is_err(),
            "the gate admitted a {side}x{side} bitstream inside a 33x17 canvas"
        );
    }
}

/// Differential: whatever the gate admits must stay under the per-image cap
/// when really decoded, by the validator's path and by the ingestion path, and
/// under the gate's own charge for the decode.
#[test]
fn what_the_gate_admits_decodes_within_its_charge_and_the_cap() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut admitted, mut refused) = (0, 0);
    let mut table = String::new();
    for fixture in fixtures() {
        let name = fixture.name;
        if gate(name, fixture.bytes).is_err() {
            refused += 1;
            table.push_str(&format!("{name:34} refused by the gate\n"));
            continue;
        }
        admitted += 1;

        let (decoded, decode_peak) =
            peak_of(|| decode_like_the_validator(fixture.shape, fixture.bytes));
        decoded.unwrap_or_else(|e| panic!("{name}: the validator's decode failed: {e}"));
        let (ingested, ingest_peak) = peak_of(|| ingest_like_webp_to_png(fixture.bytes));
        ingested.unwrap_or_else(|e| panic!("{name}: webp_to_png's decode failed: {e}"));

        table.push_str(&format!(
            "{name:34} charged {:>10}  validator decode {:>10}  ingest+PNG {:>10}\n",
            fixture.charged(),
            decode_peak,
            ingest_peak
        ));
        assert!(
            decode_peak <= fixture.charged() + FIXED_DECODER_OVERHEAD,
            "{name}: decode peaked at {decode_peak} bytes, above the {} the gate charges
{table}",
            fixture.charged()
        );
        assert!(
            decode_peak <= PER_IMAGE_CAP && ingest_peak <= PER_IMAGE_CAP,
            "{name}: peak above the per-image cap (decode {decode_peak}, ingest {ingest_peak})\n{table}"
        );
    }
    eprintln!("{table}");
    assert!(admitted >= 10, "only {admitted} fixtures were admitted");
    assert!(
        refused >= 1,
        "no fixture was refused: the cap is not exercised"
    );
}

/// Every real file, with its VP8 header enlarged, is refused from the header
/// alone (nothing here decodes, so nothing large is allocated).
#[test]
fn every_fixture_with_an_enlarged_bitstream_header_is_refused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for fixture in fixtures() {
        for side in [4096u16, 8192, 16383] {
            let hostile = with_vp8_header_of(fixture.bytes, side);
            let name = format!("{}@{side}", fixture.name);
            let error = gate(&name, &hostile)
                .err()
                .unwrap_or_else(|| panic!("{name} was admitted"));
            if fixture.shape != Shape::Still {
                assert!(
                    error
                        .to_string()
                        .contains(&format!("declares {side}x{side}")),
                    "{name}: refused for another reason: {error}"
                );
            }
        }
    }
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Sides that sit on a boundary of the format or of the gate: 14-bit limit,
/// 16-pixel macroblocks, the dimension ceiling, and the sides at which a still
/// with and without alpha crosses the 64 MiB cap.
const SIDES: [u32; 24] = [
    0, 1, 2, 15, 16, 17, 255, 256, 1023, 1024, 2047, 2048, 2364, 2365, 2729, 2730, 2896, 2897,
    4095, 4096, 8191, 8192, 16382, 16383,
];

fn set_u24(bytes: &mut [u8], at: usize, minus_one: u32) {
    bytes[at..at + 3].copy_from_slice(&minus_one.to_le_bytes()[..3]);
}

/// One mutated copy of `fixture` and a description of what was changed.
fn mutate(fixture: &Fixture, rng: &mut XorShift) -> (Vec<u8>, String) {
    let mut out = fixture.bytes.to_vec();
    let at = vp8_header_at(&out);
    let side = |rng: &mut XorShift| {
        if rng.below(4) == 0 {
            (rng.next() & 0x3fff) as u32
        } else {
            SIDES[rng.below(SIDES.len())].min(16383)
        }
    };
    let (header_w, header_h, scale_w, scale_h, canvas);
    match rng.below(3) {
        // A hostile bitstream header; the container is left as it was.
        0 => {
            header_w = side(rng);
            header_h = side(rng);
            scale_w = (rng.below(4) as u16) << 14;
            scale_h = (rng.below(4) as u16) << 14;
            canvas = None;
        }
        // Header and container agree on a size: nothing for the canvas check
        // to refuse, so only the projection stands between the file and the
        // allocation.
        1 => {
            let s = side(rng);
            header_w = s;
            header_h = s;
            scale_w = 0;
            scale_h = 0;
            canvas = Some((s, s));
        }
        // A header left alone inside a container that claims something else.
        _ => {
            header_w = u32::from(u16::from_le_bytes([out[at], out[at + 1]]) & 0x3fff);
            header_h = u32::from(u16::from_le_bytes([out[at + 2], out[at + 3]]) & 0x3fff);
            scale_w = 0;
            scale_h = 0;
            canvas = Some((side(rng), side(rng)));
        }
    }
    out[at..at + 2].copy_from_slice(&((header_w as u16 & 0x3fff) | scale_w).to_le_bytes());
    out[at + 2..at + 4].copy_from_slice(&((header_h as u16 & 0x3fff) | scale_h).to_le_bytes());
    if let Some((w, h)) = canvas {
        if &out[12..16] == b"VP8X" {
            set_u24(&mut out, 24, w.max(1) - 1);
            set_u24(&mut out, 27, h.max(1) - 1);
        }
        if let Some(anmf) = out.windows(4).position(|window| window == b"ANMF") {
            set_u24(&mut out, anmf + 8 + 6, w.max(1) - 1);
            set_u24(&mut out, anmf + 8 + 9, h.max(1) - 1);
        }
    }
    let what = format!(
        "{} header {header_w}x{header_h} scale {}/{} canvas {canvas:?}",
        fixture.name,
        scale_w >> 14,
        scale_h >> 14
    );
    (out, what)
}

/// A seeded sweep over hostile and mismatched headers on every fixture. The
/// property: if the gate admits a file whose dimensions the sweep mutated,
/// decoding it never holds more than the per-image cap. It covers the
/// dimension mechanism only (VP8 header, VP8X canvas, ANMF rectangle, scale
/// bits) over the 16 fixtures; it does not cover allocations that grow with the
/// bitstream's content (VP8L Huffman groups, transform data), which the
/// per-pixel charge does not model. The seed is fixed, so a failure reproduces;
/// the message carries the iteration and the mutation.
#[test]
fn nothing_the_gate_admits_decodes_above_the_cap_across_a_seeded_sweep() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    const SEED: u64 = 0x9883_0000_c0ff_ee01;
    const ITERATIONS: usize = 400;
    let mut rng = XorShift(SEED);
    let fixtures = fixtures();
    let (mut admitted, mut refused, mut sizeable, mut unchanged) = (0usize, 0usize, 0usize, 0usize);
    let mut largest = 0u64;
    for iteration in 0..ITERATIONS {
        let fixture = &fixtures[rng.below(fixtures.len())];
        let (bytes, what) = mutate(fixture, &mut rng);
        if bytes == fixture.bytes {
            // The draw landed on the fixture's own values: not a mutation.
            unchanged += 1;
            continue;
        }
        if gate("sweep.webp", &bytes).is_err() {
            refused += 1;
            continue;
        }
        admitted += 1;
        let (_, ingest) = peak_of(|| ingest_like_webp_to_png(&bytes));
        let (_, validator) = peak_of(|| decode_like_the_validator(fixture.shape, &bytes));
        let peak = ingest.max(validator);
        largest = largest.max(peak);
        if peak > MIB {
            sizeable += 1;
        }
        assert!(
            peak <= PER_IMAGE_CAP,
            "iteration {iteration} (seed {SEED:#x}): the gate admitted a file that decodes at {} MiB \
             (ingest {ingest}, validator {validator}): {what}",
            peak / MIB
        );
    }
    eprintln!(
        "sweep: {admitted} admitted, {refused} refused, {unchanged} unchanged (skipped), \
         {sizeable} admitted above 1 MiB, largest admitted decode {} KiB",
        largest / 1024
    );
    // Without these the sweep could pass by never reaching the gate's edge.
    assert!(admitted >= 100, "only {admitted} mutations were admitted");
    assert!(refused >= 100, "only {refused} mutations were refused");
    assert!(
        sizeable >= 20,
        "only {sizeable} admitted decodes were above 1 MiB"
    );
}

/// The case the charge for stills with alpha exists for. Charged at 4 + 5 B/px
/// it was admitted (2600 x 2600 x 9 is under 64 MiB), yet decoding it holds the
/// VP8 planes, the RGBA output, and `read_alpha_chunk`'s buffers together:
/// about 10.5 B/px, 71 MB, over the cap. Predicted before the first run.
#[test]
fn an_alpha_still_that_really_decodes_above_the_cap_is_refused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let bytes = include_bytes!("testdata/lossy_alpha_2600x2600.webp");

    let (decoded, peak) = peak_of(|| ingest_like_webp_to_png(bytes));
    decoded.expect("the real file decodes");
    assert!(
        peak > PER_IMAGE_CAP,
        "the premise of this test is gone: the decode peaked at {} MiB, under the cap",
        peak / MIB
    );
    let error = gate("alpha-2600.webp", bytes).err().unwrap_or_else(|| {
        panic!(
            "the gate admitted a file that decodes at {} MiB",
            peak / MIB
        )
    });
    assert!(error.to_string().contains("per-image limit"), "{error}");
}

/// Mirrors `WEBP_VP8_INPUT_BYTES_MULTIPLIER` in `multimodal.rs`, which is
/// private.
const VP8_INPUT_BYTES_MULTIPLIER: u64 = 5;

/// `bytes` with `extra` zero bytes added inside its (bare, first) `VP8 ` chunk.
/// The bitstream reader ignores what follows the frame it needs, so the file
/// still decodes; only the size of the chunk changes.
fn with_padded_vp8_chunk(bytes: &[u8], extra: usize) -> Vec<u8> {
    assert_eq!(&bytes[12..16], b"VP8 ", "a bare VP8 file is expected");
    let size = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as usize;
    let mut payload = bytes[20..20 + size].to_vec();
    payload.resize(size + extra, 0);
    let mut chunk = b"VP8 ".to_vec();
    chunk.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    chunk.extend_from_slice(&payload);
    if payload.len() % 2 == 1 {
        chunk.push(0);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&((chunk.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&chunk);
    out
}

/// What the charge for the size of a `VP8 ` chunk exists for. A 2048 x 2048
/// still is charged 33,554,432 B by its pixels; padded to 19 MB inside the
/// chunk it decoded at 71,449,067 B, over the cap, and was admitted.
/// Predicted before the first run: peak above the cap, and refused now.
#[test]
fn a_padded_vp8_chunk_that_decodes_above_the_cap_is_refused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let padded = with_padded_vp8_chunk(
        include_bytes!("testdata/lossy_still_2048x2048.webp"),
        19_000_000,
    );

    let (decoded, peak) = peak_of(|| ingest_like_webp_to_png(&padded));
    decoded.expect("the padded file decodes");
    assert!(
        peak > PER_IMAGE_CAP,
        "the premise of this test is gone: the decode peaked at {peak} bytes, under the cap"
    );
    let error = gate("padded.webp", &padded)
        .err()
        .unwrap_or_else(|| panic!("the gate admitted a file that decodes at {peak} bytes"));
    assert!(error.to_string().contains("per-image limit"), "{error}");
}

/// The multiplier is measured, not derived: for chunks of several sizes, the
/// decode holds the chunk at most three times over, and the gate charges more
/// than that. The sizes include ones just above a power of two, where growth by
/// doubling overshoots the most. Predicted before the first run: at most 3.0
/// times the chunk, and never above the charge.
#[test]
fn a_vp8_chunk_is_held_no_more_than_the_gate_charges_for() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let base = include_bytes!("testdata/lossy_still_64x64.webp");
    let fixture = fixtures()
        .into_iter()
        .find(|f| f.name == "lossy_still_64x64.webp")
        .expect("fixture is listed");
    let (_, unpadded) = peak_of(|| ingest_like_webp_to_png(base));
    let mut worst = 0f64;
    for extra in [
        100_000usize,
        1 << 20,
        (1 << 20) + 1,
        3_000_000,
        (1 << 22) + 1,
        (1 << 23) + 1,
        16_777_217,
    ] {
        let padded = with_padded_vp8_chunk(base, extra);
        let chunk = u64::from(u32::from_le_bytes([
            padded[16], padded[17], padded[18], padded[19],
        ]));
        let (ingested, ingest) = peak_of(|| ingest_like_webp_to_png(&padded));
        let (validated, validator) = peak_of(|| decode_like_the_validator(Shape::Still, &padded));
        ingested.expect("the padded file decodes");
        validated.expect("the padded file decodes");
        let peak = ingest.max(validator);
        worst = worst.max((peak as f64 - unpadded as f64) / chunk as f64);
        assert!(
            peak <= fixture.charged() + VP8_INPUT_BYTES_MULTIPLIER * chunk,
            "chunk of {chunk} bytes: decode peaked at {peak}, above the charge"
        );
        assert!(
            gate("padded.webp", &padded).is_ok()
                == (fixture.charged() + VP8_INPUT_BYTES_MULTIPLIER * chunk < PER_IMAGE_CAP),
            "chunk of {chunk} bytes: the gate and the charge disagree on the cap"
        );
    }
    eprintln!("worst (peak - unpadded peak) / chunk = {worst:.3}");
    assert!(
        worst <= 3.05,
        "a chunk is held {worst:.3} times over, above the 3.0 measured"
    );
}

/// A lossless still with alpha decodes straight into its 4 B/px output: at
/// 2500 x 2500 a 4 + 8 charge (75 MB) would refuse it, and the real decode is
/// about 25 MB. The charge of 8 is for the lossy-with-ALPH path; a lossless
/// file must not pay it. Predicted before the first run: admitted, peak near
/// 4 B/px, well under the charge.
///
/// The image is one flat colour, so the PNG that the ingest builds compresses
/// to almost nothing and `ingest` here is the decode. With uniformly random
/// pixels the same ingest was measured at 18 B/px (127 MiB at 2730 px), twice
/// the cap, which the charge does not model: the PNG-encode residual.
#[test]
fn a_lossless_alpha_still_is_not_charged_like_a_lossy_one() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let side = 2500u32;
    let bytes = {
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            side,
            side,
            image::Rgba([255, 0, 0, 128]),
        ))
        .write_to(&mut cursor, image::ImageFormat::WebP)
        .expect("the lossless encoder writes a WebP");
        cursor.into_inner()
    };
    let charged = u64::from(side) * u64::from(side) * (4 + 5);

    gate("lossless-alpha.webp", &bytes)
        .expect("a lossless alpha still of 2500 px is under the cap and must be admitted");
    let (ingested, ingest) = peak_of(|| ingest_like_webp_to_png(&bytes));
    let (validated, validator) = peak_of(|| decode_like_the_validator(Shape::Alpha, &bytes));
    ingested.expect("the file decodes");
    validated.expect("the file decodes");
    assert!(
        validator <= charged,
        "the validator's decode peaked at {validator} bytes, above the {charged} charged"
    );
    assert!(
        ingest <= PER_IMAGE_CAP && validator <= PER_IMAGE_CAP,
        "ingest {ingest}, validator {validator}: above the cap"
    );
    assert!(
        validator < u64::from(side) * u64::from(side) * (4 + 8) / 2,
        "the premise is gone: the decode holds {validator} bytes, not about 4 B/px"
    );
}

/// A lossy still whose ALPH plane is lossless with predictor and colour
/// transforms (2469 x 2469; a crafted file, not a camera or encoder default).
/// Its transform data is held on top of the 10.5 B/px of a plain alpha plane:
/// measured about 11.02 B/px, 67,144,965 B in a build of this tree before the
/// charge went from 4 + 7 to 4 + 8. At 4 + 7 it was charged 67,055,571 B, under
/// the cap, and decoded above it. Predicted before the first run.
#[test]
fn an_alpha_still_with_transforms_that_decodes_above_the_cap_is_refused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let bytes = include_bytes!("testdata/alpha_xforms_2469x2469.webp");

    let (decoded, peak) = peak_of(|| ingest_like_webp_to_png(bytes));
    decoded.expect("the file decodes");
    assert!(
        peak > PER_IMAGE_CAP,
        "the premise of this test is gone: the decode peaked at {peak} bytes, under the cap"
    );
    let error = gate("alpha-xforms.webp", bytes)
        .err()
        .unwrap_or_else(|| panic!("the gate admitted a file that decodes at {peak} bytes"));
    assert!(error.to_string().contains("per-image limit"), "{error}");
}
