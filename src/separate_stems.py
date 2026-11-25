"""Séparation d'un morceau en pistes (stems) avec Demucs, pour Rustymix.

Usage : python separate_stems.py ENTREE.f32 DOSSIER_SORTIE [MODELE]

ENTREE.f32 : audio stéréo entrelacé, float32 petit-boutiste, 44,1 kHz (écrit
par Rustymix avec son propre décodeur, pour rester aligné sur le mix joué).
Écrit vocals.flac, drums.flac et bass.flac (16 bits, 44,1 kHz) dans le dossier
de sortie ; le reste du mix (« instruments ») est déduit par Rustymix.

Sorties sur stdout, une par ligne :
  PROGRESS 0.42       avancement (0 à 1)
  DEVICE cuda         processeur utilisé
"""

import os
import sys

import numpy as np
import soundfile
import torch
import demucs.apply
from demucs.apply import apply_model
from demucs.pretrained import get_model

SAMPLE_RATE = 44100
KEPT = ("vocals", "drums", "bass")


class Progress:
    """Remplace la barre tqdm de Demucs par des lignes « PROGRESS x »."""

    def __init__(self, iterable, **_kwargs):
        self.items = list(iterable)

    def __iter__(self):
        total = max(len(self.items), 1)
        for done, item in enumerate(self.items, start=1):
            yield item
            print(f"PROGRESS {done / total:.3f}", flush=True)


def main():
    source, out_dir = sys.argv[1], sys.argv[2]
    model_name = sys.argv[3] if len(sys.argv) > 3 else "htdemucs"

    demucs.apply.tqdm.tqdm = Progress
    device = "cuda" if torch.cuda.is_available() else "cpu"
    print(f"DEVICE {device}", flush=True)
    if device == "cuda":
        # Rustymix dessine son interface sur la même carte graphique : on
        # laisse de la mémoire vidéo libre (sans quoi l'affichage peut échouer).
        torch.cuda.set_per_process_memory_fraction(0.6)

    model = get_model(model_name)
    model.eval()
    if model.samplerate != SAMPLE_RATE:
        sys.exit(f"modèle à {model.samplerate} Hz non pris en charge")

    raw = np.fromfile(source, dtype="<f4")
    wav = torch.from_numpy(raw.reshape(-1, 2).T.copy())
    # Normalisation identique à `demucs.separate`.
    ref = wav.mean(0)
    mean, std = ref.mean(), ref.std() + 1e-8
    wav = (wav - mean) / std

    with torch.no_grad():
        sources = apply_model(model, wav[None], device=device, shifts=1, split=True, overlap=0.25, progress=True)[0]
    sources = sources * std + mean

    os.makedirs(out_dir, exist_ok=True)
    for name, stem in zip(model.sources, sources):
        if name not in KEPT:
            continue
        final = os.path.join(out_dir, f"{name}.flac")
        partial = final + ".part"
        data = np.clip(stem.cpu().numpy().T, -1.0, 1.0)
        soundfile.write(partial, data, SAMPLE_RATE, subtype="PCM_16", format="FLAC")
        os.replace(partial, final)
    print("PROGRESS 1.000", flush=True)


if __name__ == "__main__":
    main()
