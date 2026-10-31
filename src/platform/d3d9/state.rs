//! The device's render, texture stage and sampler states as the game set them, so that
//! GetRenderState, GetTextureStageState and GetSamplerState are answered without waiting for
//! the worker thread (`queue`). Filled from the host's device after CreateDevice and Reset and
//! after a state block's Apply (`device::fill_states`); until then (`valid` false) a Get asks
//! the host.

use std::sync::Mutex;

const RENDER: usize = 256;
const STAGES: usize = 8;
const STAGE_STATES: usize = 33;
const SAMPLERS: usize = 16;
const SAMPLER_STATES: usize = 14;

pub struct States {
    render: [u32; RENDER],
    stage: [[u32; STAGE_STATES]; STAGES],
    sampler: [[u32; SAMPLER_STATES]; SAMPLERS],
    valid: bool,
}

impl Default for States {
    fn default() -> Self {
        States { render: [0; RENDER], stage: [[0; STAGE_STATES]; STAGES], sampler: [[0; SAMPLER_STATES]; SAMPLERS], valid: false }
    }
}

impl States {
    pub fn set_render(&mut self, k: u32, v: u32) {
        if let Some(x) = self.render.get_mut(k as usize) {
            *x = v;
        }
    }

    pub fn render(&self, k: u32) -> Option<u32> {
        self.render.get(k as usize).copied().filter(|_| self.valid)
    }

    pub fn set_stage(&mut self, stage: u32, k: u32, v: u32) {
        if let Some(x) = self.stage.get_mut(stage as usize).and_then(|s| s.get_mut(k as usize)) {
            *x = v;
        }
    }

    pub fn stage(&self, stage: u32, k: u32) -> Option<u32> {
        self.stage.get(stage as usize).and_then(|s| s.get(k as usize)).copied().filter(|_| self.valid)
    }

    pub fn set_sampler(&mut self, s: u32, k: u32, v: u32) {
        if let Some(x) = self.sampler.get_mut(s as usize).and_then(|s| s.get_mut(k as usize)) {
            *x = v;
        }
    }

    pub fn sampler(&self, s: u32, k: u32) -> Option<u32> {
        self.sampler.get(s as usize).and_then(|s| s.get(k as usize)).copied().filter(|_| self.valid)
    }

    /// The cache holds the host's values (after a fill).
    pub fn validate(&mut self) {
        self.valid = true;
    }

    /// The host's states changed behind the cache (a state block's Apply).
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    pub fn valid(&self) -> bool {
        self.valid
    }

    /// Every index a fill reads: (render states), (stage, state), (sampler, state).
    pub fn sizes() -> (u32, (u32, u32), (u32, u32)) {
        (RENDER as u32, (STAGES as u32, STAGE_STATES as u32), (SAMPLERS as u32, SAMPLER_STATES as u32))
    }
}

/// The device's states.
pub static STATES: Mutex<Option<States>> = Mutex::new(None);
