use bootui::{ArcSpinnerStyle, Color, Image, PixelFormat, Point, Rect, Surface};
use core::ffi::c_void;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use uefi::table::boot::{BootServices, EventType, TimerTrigger, Tpl};
use uefi::Event;

include!(concat!(env!("OUT_DIR"), "/boot_logo.rs"));

static ADDRESS: AtomicUsize = AtomicUsize::new(0);
static LENGTH: AtomicUsize = AtomicUsize::new(0);
static WIDTH: AtomicUsize = AtomicUsize::new(0);
static HEIGHT: AtomicUsize = AtomicUsize::new(0);
static STRIDE: AtomicUsize = AtomicUsize::new(0);
static FORMAT: AtomicU8 = AtomicU8::new(0);
static PHASE: AtomicU8 = AtomicU8::new(0);
static RENDERING: AtomicBool = AtomicBool::new(false);

pub fn initialize(
    address: usize,
    length: usize,
    width: usize,
    height: usize,
    stride: usize,
    format: PixelFormat,
) -> bool {
    let Some(required) = stride
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return false;
    };
    if address == 0 || width == 0 || height == 0 || stride < width || length < required {
        return false;
    }
    ADDRESS.store(address, Ordering::Release);
    LENGTH.store(length, Ordering::Relaxed);
    WIDTH.store(width, Ordering::Relaxed);
    HEIGHT.store(height, Ordering::Relaxed);
    STRIDE.store(stride, Ordering::Relaxed);
    FORMAT.store(
        match format {
            PixelFormat::Rgb => 1,
            PixelFormat::Bgr => 2,
        },
        Ordering::Relaxed,
    );
    true
}

pub fn show_loading() {
    let _ = with_surface(|surface| {
        surface.clear(Color::BLACK);
        let Ok(logo) = Image::new(
            BOOT_LOGO_RGBA,
            BOOT_LOGO_WIDTH,
            BOOT_LOGO_HEIGHT,
            BOOT_LOGO_WIDTH as usize * 4,
        ) else {
            return;
        };
        let longest = (surface.width() / 8).min(surface.height() / 8).max(1);
        let source_longest = BOOT_LOGO_WIDTH.max(BOOT_LOGO_HEIGHT);
        let width =
            (u64::from(BOOT_LOGO_WIDTH) * u64::from(longest) / u64::from(source_longest)) as u32;
        let height =
            (u64::from(BOOT_LOGO_HEIGHT) * u64::from(longest) / u64::from(source_longest)) as u32;
        let x = i64::from(surface.width()) / 2 - i64::from(width) / 2;
        let y = i64::from(surface.height()) * 38 / 100 - i64::from(height) / 2;
        surface.draw_image_scaled(
            logo,
            Rect::new(to_i32(x), to_i32(y), width.max(1), height.max(1)),
        );
        draw_spinner(surface, 0);
    });
}

pub fn show_error() {
    let _ = with_surface(|surface| {
        surface.clear(Color::BLACK);
        let card_width = surface.width().min(520).saturating_sub(32).max(180);
        let card_height = 180_u32.min(surface.height().saturating_sub(32)).max(100);
        let x = to_i32(i64::from(surface.width().saturating_sub(card_width)) / 2);
        let y = to_i32(i64::from(surface.height().saturating_sub(card_height)) / 2);
        surface.fill_rounded_rect(
            Rect::new(x, y, card_width, card_height),
            18,
            Color::rgb(246, 247, 249),
        );
        let center = Point::new(x + 52, y + i32::try_from(card_height / 2).unwrap_or(50));
        surface.fill_circle(center, 22, Color::rgb(224, 47, 41));
        surface.fill_rounded_rect(
            Rect::new(center.x - 2, center.y - 12, 4, 17),
            2,
            Color::WHITE,
        );
        surface.fill_circle(Point::new(center.x, center.y + 11), 2, Color::WHITE);
    });
}

pub struct LoadingAnimation<'boot> {
    boot_services: &'boot BootServices,
    event: Option<Event>,
}

impl LoadingAnimation<'_> {
    pub fn stop(mut self) {
        self.close();
    }

    fn close(&mut self) {
        if let Some(event) = self.event.take() {
            let _ = self.boot_services.set_timer(&event, TimerTrigger::Cancel);
            let _ = self.boot_services.close_event(event);
        }
    }
}

impl Drop for LoadingAnimation<'_> {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn start_loading_animation(boot_services: &BootServices) -> Option<LoadingAnimation<'_>> {
    unsafe extern "efiapi" fn tick(_event: Event, _context: Option<NonNull<c_void>>) {
        advance_loading();
    }
    let event = unsafe {
        boot_services.create_event(
            EventType::TIMER | EventType::NOTIFY_SIGNAL,
            Tpl::CALLBACK,
            Some(tick),
            None,
        )
    }
    .ok()?;
    if boot_services
        .set_timer(&event, TimerTrigger::Periodic(800_000))
        .is_err()
    {
        let _ = boot_services.close_event(event);
        return None;
    }
    Some(LoadingAnimation {
        boot_services,
        event: Some(event),
    })
}

/// Advances the loading indicator when firmware is busy in a synchronous
/// operation and cannot dispatch timer events.
pub fn advance_loading() {
    let phase = PHASE.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    let _ = with_surface(|surface| {
        let center = spinner_center(surface);
        surface.fill_rect(
            Rect::new(center.x - 12, center.y - 12, 24, 24),
            Color::BLACK,
        );
        draw_spinner(surface, phase);
    });
}

fn draw_spinner(surface: &mut Surface<'_>, phase: u8) {
    surface.draw_arc_spinner(
        spinner_center(surface),
        phase,
        ArcSpinnerStyle::new(8, 2, Color::rgb(244, 246, 250)),
    );
}

fn spinner_center(surface: &Surface<'_>) -> Point {
    Point::new(
        i32::try_from(surface.width() / 2).unwrap_or(i32::MAX),
        i32::try_from(surface.height() * 3 / 5).unwrap_or(i32::MAX),
    )
}

fn with_surface(draw: impl FnOnce(&mut Surface<'_>)) -> bool {
    if RENDERING
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return false;
    }
    let rendered = (|| {
        let address = ADDRESS.load(Ordering::Acquire);
        let length = LENGTH.load(Ordering::Relaxed);
        let width = u32::try_from(WIDTH.load(Ordering::Relaxed)).ok()?;
        let height = u32::try_from(HEIGHT.load(Ordering::Relaxed)).ok()?;
        let stride = u32::try_from(STRIDE.load(Ordering::Relaxed)).ok()?;
        let format = match FORMAT.load(Ordering::Relaxed) {
            1 => PixelFormat::Rgb,
            2 => PixelFormat::Bgr,
            _ => return None,
        };
        if address == 0 {
            return None;
        }
        let pixels = unsafe { core::slice::from_raw_parts_mut(address as *mut u8, length) };
        let mut surface = Surface::new(pixels, width, height, stride, format).ok()?;
        draw(&mut surface);
        Some(())
    })()
    .is_some();
    RENDERING.store(false, Ordering::Release);
    rendered
}

fn to_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}
