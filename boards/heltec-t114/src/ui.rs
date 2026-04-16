//! Heltec T114-specific UI bindings for `reticulum-node::ui`.
//!
//! This module owns all board-specific display/input details so the
//! `reticulum-node` crate stays hardware-agnostic.

use core::convert::Infallible;

use reticulum_node::ui::{
    DisplayOrientation, DisplayProfile, GpioPin, InterfacesScreen, OverviewScreen, SpiDisplayPins,
    StatusDataSource, StatusScreen, StatusScreenSet, StatusSnapshot, UiAction, UiFrame, UiInput,
    UiRect,
};

/// Display profile for the Heltec Mesh Node T114 panel.
///
/// Source references:
/// - Meshtastic: `variants/nrf52840/heltec_mesh_node_t114/variant.h`
/// - Heltec panel sheet: `1.14inch LH114T-IF03 VER C.pdf`
pub struct HeltecT114Profile;

impl DisplayProfile for HeltecT114Profile {
    const NAME: &'static str = "Heltec Mesh Node T114";
    const CONTROLLER: &'static str = "ST7789V2";
    const WIDTH: u16 = 135;
    const HEIGHT: u16 = 240;
    const OFFSET_X: u16 = 52;
    const OFFSET_Y: u16 = 40;
    const SPI_HZ: u32 = 40_000_000;
    const ORIENTATION: DisplayOrientation = DisplayOrientation::Portrait;
}

impl HeltecT114Profile {
    /// Display wiring in nRF `(port, pin)` form.
    pub const DISPLAY_PINS: SpiDisplayPins = SpiDisplayPins {
        sck: GpioPin::new(1, 8),
        mosi: GpioPin::new(1, 9),
        cs: GpioPin::new(0, 11),
        dc: GpioPin::new(0, 12),
        rst: GpioPin::new(0, 2),
        backlight: GpioPin::new(0, 15),
    };

    /// Primary UI button used to advance pages.
    pub const BUTTON1: GpioPin = GpioPin::new(1, 10);

    /// ST7789 rotation index typically used for landscape dashboard rendering.
    pub const LANDSCAPE_ROTATION: u8 = 1;
}

/// Single-button input adapter.
///
/// On a press edge this emits [`UiAction::NextTab`], which matches the T114's
/// one-button UX used by Meshtastic.
pub struct SingleButtonInput<F>
where
    F: FnMut() -> bool,
{
    read_pressed: F,
    was_pressed: bool,
}

impl<F> SingleButtonInput<F>
where
    F: FnMut() -> bool,
{
    /// Construct from a closure that returns whether the button is currently pressed.
    pub const fn new(read_pressed: F) -> Self {
        Self {
            read_pressed,
            was_pressed: false,
        }
    }
}

impl<F> UiInput for SingleButtonInput<F>
where
    F: FnMut() -> bool,
{
    type Error = Infallible;

    fn poll_action(&mut self) -> Result<Option<UiAction>, Self::Error> {
        let pressed = (self.read_pressed)();
        let action = if pressed && !self.was_pressed {
            Some(UiAction::NextTab)
        } else {
            None
        };
        self.was_pressed = pressed;
        Ok(action)
    }
}

/// Default T114 screen collection (overview + interfaces).
#[derive(Default)]
pub struct T114ScreenSet {
    overview: OverviewScreen,
    interfaces: InterfacesScreen,
}

impl<const N_IFACES: usize, const N_EVENTS: usize> StatusScreenSet<N_IFACES, N_EVENTS>
    for T114ScreenSet
{
    fn len(&self) -> usize {
        2
    }

    fn title(&self, index: usize) -> &'static str {
        match index {
            0 => self.overview.title(),
            1 => self.interfaces.title(),
            _ => "",
        }
    }

    fn render(
        &mut self,
        index: usize,
        frame: &mut UiFrame<'_>,
        area: UiRect,
        snapshot: &StatusSnapshot<N_IFACES, N_EVENTS>,
    ) {
        match index {
            0 => self.overview.render(frame, area, snapshot),
            1 => self.interfaces.render(frame, area, snapshot),
            _ => self.overview.render(frame, area, snapshot),
        }
    }
}

/// Minimal board-local status source wrapper.
pub struct T114StatusSource<D, const N_IFACES: usize, const N_EVENTS: usize>
where
    D: Fn() -> StatusSnapshot<N_IFACES, N_EVENTS>,
{
    read_snapshot: D,
}

impl<D, const N_IFACES: usize, const N_EVENTS: usize> T114StatusSource<D, N_IFACES, N_EVENTS>
where
    D: Fn() -> StatusSnapshot<N_IFACES, N_EVENTS>,
{
    /// Construct from a snapshot provider closure.
    pub const fn new(read_snapshot: D) -> Self {
        Self { read_snapshot }
    }
}

impl<D, const N_IFACES: usize, const N_EVENTS: usize> StatusDataSource<N_IFACES, N_EVENTS>
    for T114StatusSource<D, N_IFACES, N_EVENTS>
where
    D: Fn() -> StatusSnapshot<N_IFACES, N_EVENTS>,
{
    fn snapshot(&self) -> StatusSnapshot<N_IFACES, N_EVENTS> {
        (self.read_snapshot)()
    }
}
