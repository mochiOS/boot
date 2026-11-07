//! ブートローダー用シリアルコンソール

use core::fmt;
use core::fmt::Write;
use spin::Mutex;
use x86_64::instructions::port::Port;

/// ブートフェーズ専用Serialコンソール
pub struct SerialConsole {
    base: u16,
}

pub static SERIAL: Mutex<Option<SerialConsole>> = Mutex::new(None);

pub fn init() -> Result<(), log::SetLoggerError> {
    let mut serial = SerialConsole::new(0x3F8);
    serial.init();
    *SERIAL.lock() = Some(serial);
    log::set_logger(&LOGGER)?;
    log::set_max_level(log::LevelFilter::Info);
    Ok(())
}

struct SerialLogger;
static LOGGER: SerialLogger = SerialLogger;

impl log::Log for SerialLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            _serial_print(format_args!(
                "[UEFI {}] {}\n",
                record.level(),
                record.args()
            ));
        }
    }

    fn flush(&self) {}
}

pub fn _serial_print(args: fmt::Arguments) {
    if let Some(serial) = SERIAL.lock().as_mut() {
        let _ = serial.write_fmt(args);
    }
}

impl SerialConsole {
    pub const fn new(base: u16) -> Self {
        Self { base }
    }

    pub fn init(&mut self) {
        unsafe {
            Port::<u8>::new(self.base + 1).write(0x00);
            Port::<u8>::new(self.base + 3).write(0x80);
            Port::<u8>::new(self.base + 0).write(0x03);
            Port::<u8>::new(self.base + 1).write(0x00);
            Port::<u8>::new(self.base + 3).write(0x03);
            Port::<u8>::new(self.base + 2).write(0xC7);
            Port::<u8>::new(self.base + 4).write(0x0B);
        }
    }

    fn is_transmit_empty(&self) -> bool {
        unsafe {
            let mut line_status = Port::<u8>::new(self.base + 5);
            (line_status.read() & 0x20) != 0
        }
    }

    fn write_byte(&mut self, byte: u8) {
        while !self.is_transmit_empty() {}

        unsafe {
            Port::<u8>::new(self.base).write(byte);
        }
    }
}

impl fmt::Write for SerialConsole {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(byte);
        }

        Ok(())
    }
}
