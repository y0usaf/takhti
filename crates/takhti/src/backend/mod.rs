pub mod tty;
pub mod winit;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::ImportDma;

pub use tty::TtyData;
pub use winit::WinitData;

/// Active backend: winit (nested dev window) or TTY (DRM/GBM real hardware).
pub enum Backend {
    Uninit,
    Winit(WinitData),
    Tty(TtyData),
}

impl Backend {
    pub fn winit(&mut self) -> &mut WinitData {
        match self {
            Backend::Winit(data) => data,
            _ => panic!("winit backend not initialized"),
        }
    }

    #[allow(dead_code)]
    pub fn tty(&mut self) -> &mut TtyData {
        match self {
            Backend::Tty(data) => data,
            _ => panic!("tty backend not initialized"),
        }
    }

    /// Import a client dmabuf into the renderer that composites it (the
    /// primary GPU on the TTY backend).
    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        match self {
            Backend::Uninit => false,
            Backend::Winit(data) => data.backend.renderer().import_dmabuf(dmabuf, None).is_ok(),
            Backend::Tty(data) => tty::import_dmabuf(data, dmabuf),
        }
    }
}
