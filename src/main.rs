//! Rustymix — application de mix DJ simplifiée : deux platines, un crossfader,
//! un EQ 3 bandes par platine et une bibliothèque de sons.
//!
//! Organisation :
//! * [`audio`] — DSP, moteur de mixage temps réel, sortie `cpal`, décodage ;
//! * [`waveform`] — formes d'onde multi-résolution ;
//! * [`library`] — scan des dossiers, recherche et tri ;
//! * [`loader`] — chargement des morceaux en arrière-plan ;
//! * [`markers`] — marqueurs (hot cues) enregistrés par morceau ;
//! * [`stems`] — séparation en pistes (voix, batterie, basse…) par Demucs ;
//! * [`transitions`] — transitions planifiées sur les marqueurs ;
//! * [`transition_graph`] — graphe de nœuds des transitions (éditeur visuel) ;
//! * [`config`] — configuration persistante ;
//! * [`downloader`] — recherche et installation de sons (yt-dlp, spotdl) ;
//! * [`ui`] — interface egui.

// Pas de console sous Windows en version release.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod analysis;
mod audio;
mod config;
mod downloader;
mod library;
mod loader;
mod markers;
mod recommend;
mod stems;
mod suggest;
mod transition_graph;
mod transitions;
mod ui;
mod waveform;

fn main() -> eframe::Result {
    // Rendu OpenGL par défaut : si la carte graphique manque de mémoire (par
    // exemple pendant une séparation Demucs sur la même carte) ou se
    // réinitialise, il continue au lieu de planter. `RUSTYMIX_RENDERER=wgpu`
    // pour revenir au rendu wgpu.
    let renderer = match std::env::var("RUSTYMIX_RENDERER").as_deref() {
        Ok("wgpu") => eframe::Renderer::Wgpu,
        _ => eframe::Renderer::Glow,
    };
    // Logo (crabe DJ), aussi intégré à l'exécutable Windows par build.rs.
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/logo-256.png")).unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rustymix")
            .with_app_id("rustymix")
            .with_icon(icon)
            .with_inner_size([1360.0, 900.0])
            .with_min_inner_size([1300.0, 700.0]),
        renderer,
        ..Default::default()
    };
    eframe::run_native(
        "Rustymix",
        options,
        Box::new(|cc| Ok(Box::new(ui::RustymixApp::new(cc)))),
    )
}
