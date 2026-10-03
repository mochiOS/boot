use core::fmt::Write;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    if let Some(mut serial) = crate::console::SERIAL.try_lock() {
        if let Some(serial) = serial.as_mut() {
            let _ = writeln!(serial, "[mBoot] panic: {info}");
        }
    }

    crate::boot_ui::show_error();

    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}
