#![no_std]

pub mod block;
pub mod control;

pub const CONTROL_PORT: u32 = 1;
pub const CONTROL_IRQ_PORT: u32 = 2;
pub const CONTROL_RING_PORT: u32 = 3;
pub const BLOCK_RING_PORT: u32 = 4;
pub const BLOCK_DATA_PAGE: u64 = 1;
pub const BLOCK_RING_PAGE: u64 = 2;
pub const BLOCK_BUFFER_FIRST_PAGE: u64 = 3;
pub const INSTALL_METADATA_PAGE: u64 = 8;
pub const BLOCK_BUFFER_COUNT: usize = 4;
pub const DOMAIN_ID: u32 = 2;
pub const CONTROL_RING_GENERATION: u64 = 1;
pub const BLOCK_RING_GENERATION: u64 = 1;
pub const CONTROL_REQUEST_KIND: u16 = 1;
pub const CONTROL_RESPONSE_KIND: u16 = 2;
pub const BLOCK_REQUEST_KIND: u16 = 3;
pub const BLOCK_RESPONSE_KIND: u16 = 4;
