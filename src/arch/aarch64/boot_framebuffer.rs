//! Firmware-supplied scanout. The boot CPU only admits a reserved RAM range;
//! the display service uses its existing framebuffer capability to present.

use mboot::fdt::{memory::MemoryMap, Fdt, Token};
use mnu::{MemoryRegion, MemoryType};

#[derive(Clone, Copy)]
pub(super) struct Framebuffer {
    pub base: u64,
    pub bytes: u64,
    pub width: u64,
    pub height: u64,
    pub stride: u64,
    mapped_bytes: u64,
}

impl Framebuffer {
    pub(super) fn memory_region(self) -> MemoryRegion {
        MemoryRegion {
            start: self.base,
            len: self.mapped_bytes,
            region_type: MemoryType::Framebuffer,
        }
    }
}

#[derive(Default)]
struct Node<'a> {
    compatible: Option<&'a [u8]>,
    status: Option<&'a [u8]>,
    memory: Option<&'a [u8]>,
    reg: Option<&'a [u8]>,
    width: Option<&'a [u8]>,
    height: Option<&'a [u8]>,
    stride: Option<&'a [u8]>,
    format: Option<&'a [u8]>,
}

fn cell(value: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(value.try_into().ok()?))
}

fn number(value: &[u8]) -> Option<u64> {
    match value.len() {
        4 => Some(u64::from(cell(value)?)),
        8 => Some(u64::from_be_bytes(value.try_into().ok()?)),
        _ => None,
    }
}

fn set<'a>(field: &mut Option<&'a [u8]>, value: &'a [u8]) -> Option<()> {
    field.replace(value).is_none().then_some(())
}

impl Node<'_> {
    fn geometry(&self, address_cells: &[u8], size_cells: &[u8]) -> Option<Framebuffer> {
        if self.format != Some(b"x8r8g8b8\0") || self.memory.is_some() {
            return None;
        }
        let address_bytes = usize::try_from(cell(address_cells)?).ok()?.checked_mul(4)?;
        let size_bytes = usize::try_from(cell(size_cells)?).ok()?.checked_mul(4)?;
        let reg = self.reg?;
        if address_bytes.checked_add(size_bytes)? != reg.len() {
            return None;
        }
        let base = number(&reg[..address_bytes])?;
        let capacity = number(&reg[address_bytes..])?;
        let width = u64::from(cell(self.width?)?);
        let height = u64::from(cell(self.height?)?);
        let pitch = u64::from(cell(self.stride?)?);
        let bytes = pitch.checked_mul(height)?;
        let mapped_bytes = bytes.checked_add(4095)? & !4095;
        if base == 0
            || base & 4095 != 0
            || width == 0
            || height == 0
            || pitch & 3 != 0
            || pitch < width.checked_mul(4)?
            || mapped_bytes > capacity
        {
            return None;
        }
        Some(Framebuffer {
            base,
            bytes,
            width,
            height,
            stride: pitch / 4,
            mapped_bytes,
        })
    }
}

pub(super) fn admit(
    tree: &Fdt<'_>,
    map: &MemoryMap<'_>,
    image_start: u64,
    image_end: u64,
) -> Option<Framebuffer> {
    let mut chosen = false;
    let mut chosen_seen = false;
    let mut named_framebuffer = false;
    let mut address_cells = None;
    let mut size_cells = None;
    let mut ranges = None;
    let mut node = Node::default();
    let mut framebuffer = None;
    for token in tree.tokens() {
        match token.ok()? {
            Token::BeginNode { depth: 1, name } => {
                chosen = name == "chosen";
                if chosen && core::mem::replace(&mut chosen_seen, true) {
                    return None;
                }
            }
            _ if !chosen => {}
            Token::Property {
                depth: 1,
                name,
                value,
            } => match name {
                "#address-cells" => set(&mut address_cells, value)?,
                "#size-cells" => set(&mut size_cells, value)?,
                "ranges" => set(&mut ranges, value)?,
                _ => {}
            },
            Token::BeginNode { depth: 2, name } => {
                named_framebuffer = name.starts_with("framebuffer@");
                node = Node::default();
            }
            Token::Property {
                depth: 2,
                name,
                value,
            } if named_framebuffer => match name {
                "compatible" => set(&mut node.compatible, value)?,
                "status" => set(&mut node.status, value)?,
                "reg" => set(&mut node.reg, value)?,
                "memory-region" => set(&mut node.memory, value)?,
                "width" => set(&mut node.width, value)?,
                "height" => set(&mut node.height, value)?,
                "stride" => set(&mut node.stride, value)?,
                "format" => set(&mut node.format, value)?,
                _ => {}
            },
            Token::EndNode { depth: 2 } if named_framebuffer => {
                if node.compatible == Some(b"simple-framebuffer\0")
                    && matches!(node.status, None | Some(b"okay\0" | b"ok\0"))
                {
                    if framebuffer.is_some() || ranges != Some(&[]) {
                        return None;
                    }
                    framebuffer = Some(node.geometry(address_cells?, size_cells?)?);
                }
            }
            Token::EndNode { depth: 1 } => chosen = false,
            _ => {}
        }
    }
    let fb = framebuffer?;
    let end = fb.base.checked_add(fb.mapped_bytes)?;
    let mut reservations = map
        .reservations()
        .iter()
        .filter(|r| fb.base < r.region.end() && r.region.start() < end);
    let reservation = reservations.next()?;
    if end > 1 << 48
        || fb.base < image_end && image_start < end
        || !map
            .ram()
            .iter()
            .any(|r| r.start() <= fb.base && end <= r.end())
        || reservation.region.start() != fb.base
        || reservation.region.end() < end
        || reservation.reusable
        || reservations.next().is_some()
    {
        return None;
    }
    Some(fb)
}
