use bootui::{Color, Image, LoadingScreen, PixelFormat, Point, Rect, Surface};
use core::ffi::c_void;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use uefi::Event;
use uefi::table::boot::{BootServices, EventType, TimerTrigger, Tpl};

include!(concat!(env!("OUT_DIR"), "/boot_logo.rs"));

static ADDRESS: AtomicUsize = AtomicUsize::new(0);
static LENGTH: AtomicUsize = AtomicUsize::new(0);
static WIDTH: AtomicUsize = AtomicUsize::new(0);
static HEIGHT: AtomicUsize = AtomicUsize::new(0);
static STRIDE: AtomicUsize = AtomicUsize::new(0);
static FORMAT: AtomicU8 = AtomicU8::new(0);
static PHASE: AtomicU8 = AtomicU8::new(0);
static START_TICKS: AtomicU64 = AtomicU64::new(0);
static TICKS_PER_MILLISECOND: AtomicU64 = AtomicU64::new(0);
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
        if let Ok(logo) = Image::new(
            BOOT_LOGO_RGBA,
            BOOT_LOGO_WIDTH,
            BOOT_LOGO_HEIGHT,
            BOOT_LOGO_WIDTH as usize * 4,
        ) {
            LoadingScreen::new(logo).paint(surface, 0);
            PHASE.store(0, Ordering::Relaxed);
        }
    });
}

pub fn show_error() {
    TICKS_PER_MILLISECOND.store(0, Ordering::Release);
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
    let start = read_ticks();
    boot_services.stall(10_000);
    let end = read_ticks();
    START_TICKS.store(end, Ordering::Relaxed);
    TICKS_PER_MILLISECOND.store(end.saturating_sub(start).max(10) / 10, Ordering::Release);
    unsafe extern "efiapi" fn tick(_event: Event, _context: Option<NonNull<c_void>>) {
        advance_loading();
    }
    let event = unsafe {
        boot_services.create_event(
            EventType::TIMER | EventType::NOTIFY_SIGNAL,
            Tpl::NOTIFY,
            Some(tick),
            None,
        )
    }
    .ok()?;
    if boot_services
        .set_timer(&event, TimerTrigger::Periodic(333_333))
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

/// Refreshes from elapsed time, so file reads cannot speed up the animation.
pub fn advance_loading() {
    let _ = with_surface(|surface| {
        let frequency = TICKS_PER_MILLISECOND.load(Ordering::Acquire);
        if frequency == 0 {
            return;
        }
        let elapsed = read_ticks().saturating_sub(START_TICKS.load(Ordering::Relaxed)) / frequency;
        let phase = LoadingScreen::phase_at(elapsed);
        if phase != PHASE.load(Ordering::Relaxed) {
            LoadingScreen::paint_spinner(surface, elapsed);
            PHASE.store(phase, Ordering::Relaxed);
        }
    });
}

/// Leave the logo, without a frozen indicator, during the renderer handoff.
pub fn finish_loading() {
    TICKS_PER_MILLISECOND.store(0, Ordering::Release);
    let _ = with_surface(LoadingScreen::clear_spinner);
}

fn read_ticks() -> u64 {
    // SAFETY: this x86_64 firmware code runs on the BSP; LFENCE orders the
    // timestamp read. Calibration uses the firmware's Stall time source.
    unsafe {
        core::arch::x86_64::_mm_lfence();
        core::arch::x86_64::_rdtsc()
    }
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
