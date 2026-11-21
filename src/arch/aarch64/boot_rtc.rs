//! Read the reference platform's actual UTC clock for the RAM-disk boot.

use mboot::fdt::{memory::MemoryMap, Fdt, Token};
static CLOCK: spin::Once<u64> = spin::Once::new();

pub(super) fn prepare(tree: &Fdt<'_>, map: &MemoryMap<'_>) {
    let Some(base) = clock_base(tree) else { return };
    if map
        .ram()
        .iter()
        .any(|r| r.start() < base + 4096 && base < r.end())
    {
        return;
    }
    CLOCK.call_once(|| base);
    if mnu::platform::install_boot_realtime(read_utc).is_err() {
        mnu::arch::halt();
    }
}

fn clock_base(tree: &Fdt<'_>) -> Option<u64> {
    let mut address_cells = None;
    let mut size_cells = None;
    let mut found = None;
    let mut compatible = None;
    let mut reg = None;
    let mut status = None;
    for token in tree.tokens() {
        match token.ok()? {
            Token::Property {
                depth: 0,
                name: "#address-cells",
                value,
            } => address_cells = Some(value),
            Token::Property {
                depth: 0,
                name: "#size-cells",
                value,
            } => size_cells = Some(value),
            Token::BeginNode { depth: 1, .. } => (compatible, reg, status) = (None, None, None),
            Token::Property {
                depth: 1,
                name,
                value,
            } => {
                let slot = match name {
                    "compatible" => &mut compatible,
                    "reg" => &mut reg,
                    "status" => &mut status,
                    _ => continue,
                };
                if slot.replace(value).is_some() {
                    return None;
                }
            }
            Token::EndNode { depth: 1 }
                if compatible == Some(b"arm,pl031\0arm,primecell\0")
                    && matches!(status, None | Some(b"okay\0" | b"ok\0")) =>
            {
                let reg = reg?;
                if address_cells != Some(&2u32.to_be_bytes())
                    || size_cells != Some(&2u32.to_be_bytes())
                    || found.is_some()
                    || reg.len() != 16
                {
                    return None;
                }
                let base = u64::from_be_bytes(reg[..8].try_into().ok()?);
                let size = u64::from_be_bytes(reg[8..].try_into().ok()?);
                if base == 0 || base & 4095 != 0 || base >= 1 << 48 || size != 4096 {
                    return None;
                }
                found = Some(base);
            }
            _ => {}
        }
    }
    found
}

fn read_utc() -> Option<u64> {
    let physical = *CLOCK.get()?;
    // SAFETY: The firmware binding validated the register page outside RAM.
    // Boot keeps IRQs masked and these reads have no side effects.
    unsafe {
        mnu::mem::paging::with_boot_mmio_page(physical, |base| {
            let read = |offset: u64| ((base + offset) as *const u32).read_volatile();
            let identity = [0x31, 0x10, 0x14, 0, 0x0d, 0xf0, 0x05, 0xb1];
            let recognized = identity
                .into_iter()
                .enumerate()
                .all(|(i, expected)| read(0xfe0 + 4 * i as u64) == expected);
            (recognized && read(0x0c) & 1 != 0).then(|| u64::from(read(0)))
        })
    }
    .flatten()
}
