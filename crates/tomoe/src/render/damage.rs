//! Invisible element that injects damage (niri's `ExtraDamage`, made
//! physical-first).
//!
//! Effects driven by uniforms (corner radius) don't bump any surface commit
//! counter, so damage trackers would never repaint them when the parameter
//! changes. One `ExtraDamage` per window persists in `Tomoe` (stable element
//! id); bumping it damages the window's whole rect exactly once.

use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::Renderer;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Size};

#[derive(Debug, Clone)]
pub struct ExtraDamage {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<i32, Physical>,
}

impl ExtraDamage {
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            commit: CommitCounter::default(),
            geometry: Rectangle::default(),
        }
    }

    /// Damage the whole geometry on the next frame.
    pub fn damage_all(&mut self) {
        self.commit.increment();
    }

    /// The renderable instance at `geometry` (output-local physical).
    pub fn render(&self, geometry: Rectangle<i32, Physical>) -> Self {
        let mut this = self.clone();
        this.geometry = geometry;
        this
    }
}

impl Default for ExtraDamage {
    fn default() -> Self {
        Self::new()
    }
}

impl Element for ExtraDamage {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(Size::from((1., 1.)))
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }
}

impl<R: Renderer> RenderElement<R> for ExtraDamage {
    fn draw(
        &self,
        _frame: &mut R::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        _dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        Ok(())
    }
}
