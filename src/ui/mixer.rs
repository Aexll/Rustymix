//! Section mixer : volume master, vu-mètre master et crossfader.

use egui::{Align2, FontId, Id, Rect, Ui, vec2};

use super::deck::format_db;
use super::theme;
use super::widgets::{Meter, Orientation, fader, knob, take_bottom, take_left, take_right, take_top, vu_meter};
use crate::audio::dsp::knob_to_gain;
use crate::audio::engine::EngineHandle;

pub struct MixerUi {
    /// Position du crossfader : 0 = tout A, 1 = tout B.
    pub crossfader: f32,
    /// Position du potentiomètre master, 0..1 (0,5 = 0 dB).
    pub master: f32,
    meter: Meter,
}

impl Default for MixerUi {
    fn default() -> Self {
        Self {
            crossfader: 0.5,
            master: 0.5,
            meter: Meter::default(),
        }
    }
}

impl MixerUi {
    /// Synchronise l'interface et le moteur ; à appeler une fois par image.
    pub fn sync(&mut self, engine: &EngineHandle, dt: f32) {
        let shared = &engine.shared;
        shared.crossfader.store(self.crossfader);
        shared.master.store(knob_to_gain(self.master));
        self.meter
            .update([shared.master_peak[0].take(), shared.master_peak[1].take()], dt);
    }

    pub fn is_animating(&self) -> bool {
        self.meter.is_active()
    }

    /// Colonne centrale : potentiomètre et vu-mètre master.
    pub fn show_master(&mut self, ui: &mut Ui, rect: Rect) {
        ui.painter().rect_filled(rect, 8.0, theme::PANEL);
        let mut area = rect.shrink(10.0);

        let title = take_top(&mut area, 18.0);
        ui.painter().text(
            title.center(),
            Align2::CENTER_CENTER,
            "MASTER",
            FontId::proportional(13.0),
            theme::TEXT,
        );
        take_top(&mut area, 8.0);

        let knob_area = take_top(&mut area, 60.0);
        let knob_rect = Rect::from_center_size(knob_area.center(), vec2(58.0, 58.0));
        knob(
            ui,
            knob_rect,
            Id::unique("master_knob"),
            &mut self.master,
            0.5,
            theme::TEXT,
        )
        .on_hover_text("Volume master (double-clic : 0 dB)");

        let value = take_top(&mut area, 18.0);
        ui.painter().text(
            value.center(),
            Align2::CENTER_CENTER,
            format_db(knob_to_gain(self.master)),
            FontId::monospace(10.0),
            theme::TEXT_DIM,
        );
        take_top(&mut area, 6.0);

        // Vu-mètre master avec repères L / R.
        let labels = take_bottom(&mut area, 14.0);
        let meter_rect = Rect::from_center_size(area.center(), vec2(34.0, area.height()));
        vu_meter(ui, meter_rect, &self.meter);
        let painter = ui.painter();
        for (text, dx) in [("L", -9.0), ("R", 9.0)] {
            painter.text(
                labels.center() + vec2(dx, 0.0),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(10.0),
                theme::TEXT_DIM,
            );
        }
    }

    /// Bandeau du crossfader, sous les platines.
    pub fn show_crossfader(&mut self, ui: &mut Ui, rect: Rect) {
        ui.painter().rect_filled(rect, 8.0, theme::PANEL);
        let mut area = Rect::from_center_size(rect.center(), vec2(rect.width().min(460.0), rect.height() - 8.0));

        let left = take_left(&mut area, 30.0);
        let right = take_right(&mut area, 30.0);
        let painter = ui.painter();
        for (label, side, index) in [("A", left, 0), ("B", right, 1)] {
            painter.text(
                side.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(18.0),
                theme::DECK_COLORS[index],
            );
        }

        let color = theme::mix(theme::DECK_COLORS[0], theme::DECK_COLORS[1], self.crossfader);
        fader(
            ui,
            area,
            Id::unique("crossfader"),
            &mut self.crossfader,
            0.5,
            Orientation::Horizontal,
            color,
        )
        .on_hover_text("Crossfader à puissance constante (double-clic : centre)");
    }
}
