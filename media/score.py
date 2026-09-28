"""Procedural score for the launch film (SPEC section 6.3).

    uv run --with numpy --with scipy python3 media/score.py

Reads media/shots.json and writes media/out/score.wav: 48 kHz, stereo, 24-bit PCM,
exactly 75.000 s. Every sound comes from a cue in shots.json, so the picture and the
score share one clock. The only randomness is numpy.random.default_rng(20260928), so the
same shots.json always gives the same bytes.
"""

from __future__ import annotations

import json
import sys
import wave
from pathlib import Path

import numpy as np
from scipy import ndimage, signal

SR = 48_000
LENGTH_S = 75.0
N = int(round(SR * LENGTH_S))
MEDIA = Path(__file__).resolve().parent
rng = np.random.default_rng(20260928)

A2, E3, A3, CS4, E4, A4, A5 = 110.0, 164.814, 220.0, 277.183, 329.628, 440.0, 880.0


def db(x: float) -> float:
    return float(10.0 ** (x / 20.0))


def t_axis(seconds: float) -> np.ndarray:
    return np.arange(int(round(seconds * SR))) / SR


def pan_gains(pan: float) -> tuple[float, float]:
    """Constant-power pan, -1 (left) to +1 (right)."""
    a = (pan + 1.0) * np.pi / 4.0
    return float(np.cos(a)), float(np.sin(a))


def normalize_peak(x: np.ndarray) -> np.ndarray:
    peak = float(np.max(np.abs(x))) if x.size else 0.0
    return x / peak if peak > 0 else x


def place(mix: np.ndarray, sound: np.ndarray, at: float, gain_db: float, pan: float = 0.0) -> None:
    """Add a mono sound, peak-normalised to gain_db dBFS, into the stereo mix at `at` seconds."""
    start = int(round(at * SR))
    if start >= N:
        return
    s = normalize_peak(sound)[: N - start] * db(gain_db)
    left, right = pan_gains(pan)
    mix[start : start + s.size, 0] += s * left
    mix[start : start + s.size, 1] += s * right


def bandpass(x: np.ndarray, lo: float, hi: float, order: int = 4) -> np.ndarray:
    sos = signal.butter(order, [lo, hi], btype="bandpass", fs=SR, output="sos")
    return signal.sosfilt(sos, x)


def pink(n: int) -> np.ndarray:
    """Pink noise by 1/sqrt(f) shaping of white noise in the frequency domain."""
    spectrum = np.fft.rfft(rng.standard_normal(n))
    f = np.fft.rfftfreq(n, 1.0 / SR)
    f[0] = f[1]
    return np.fft.irfft(spectrum / np.sqrt(f), n)


def swept_band(x: np.ndarray, centre: np.ndarray, width_oct: float) -> np.ndarray:
    """A band-pass whose centre follows `centre` (Hz per sample), done frame by frame in the STFT."""
    nper, hop = 2048, 256
    _, times, z = signal.stft(x, fs=SR, nperseg=nper, noverlap=nper - hop, boundary="even")
    freqs = np.fft.rfftfreq(nper, 1.0 / SR)
    idx = np.clip((times * SR).astype(int), 0, centre.size - 1)
    c = centre[idx]
    lf = np.log2(np.maximum(freqs, 1.0))[:, None]
    gain = np.exp(-0.5 * ((lf - np.log2(c)[None, :]) / (width_oct / 2.0)) ** 2)
    _, y = signal.istft(z * gain, fs=SR, nperseg=nper, noverlap=nper - hop, boundary=True)
    return y[: x.size]


# Voices. Each returns a mono array; levels are set when the cue is placed.


def click(duration: float = 0.003) -> np.ndarray:
    """3 ms click through a band-pass at 2.4 kHz, with a short ring-out."""
    n = int(round(duration * SR))
    burst = np.zeros(int(0.04 * SR))
    burst[:n] = rng.standard_normal(n) * np.hanning(n)
    return bandpass(burst, 2400 / 1.25, 2400 * 1.25, order=2)


def decaying_sine(freq: float, decay: float, length: float | None = None, attack: float = 0.002) -> np.ndarray:
    t = t_axis(length if length is not None else decay * 5.0)
    env = np.exp(-t / (decay / 5.0)) * np.minimum(1.0, t / attack)
    return np.sin(2 * np.pi * freq * t) * env


def fm_bell(freq: float, decay: float) -> np.ndarray:
    """Two-operator FM bell: modulator at 1.4x the carrier, index falling with the amplitude."""
    t = t_axis(decay * 2.5)
    env = np.exp(-t * 5.0 / decay) * np.minimum(1.0, t / 0.003)
    index = 3.0 * np.exp(-t * 7.0 / decay)
    return np.sin(2 * np.pi * freq * t + index * np.sin(2 * np.pi * freq * 1.4 * t)) * env


def soft_note(freq: float, length: float, decay: float) -> np.ndarray:
    """A rounded sine note with a touch of second harmonic, for the two-note figures."""
    t = t_axis(length)
    env = np.exp(-t / decay) * np.minimum(1.0, t / 0.012)
    return (np.sin(2 * np.pi * freq * t) + 0.18 * np.sin(4 * np.pi * freq * t)) * env


def thud() -> np.ndarray:
    """60 Hz sine with a 250 ms decay; the pitch settles from 90 Hz for the attack."""
    t = t_axis(0.9)
    freq = 60.0 + 30.0 * np.exp(-t / 0.025)
    phase = 2 * np.pi * np.cumsum(freq) / SR
    return np.sin(phase) * np.exp(-t / (0.25 / 4.0)) * np.minimum(1.0, t / 0.002)


def whoosh_down(length: float = 0.5) -> np.ndarray:
    """Noise sweeping 3 kHz down to 400 Hz over 500 ms."""
    n = int(round((length + 0.25) * SR))
    t = np.arange(n) / SR
    centre = 3000.0 * (400.0 / 3000.0) ** np.clip(t / length, 0.0, 1.0)
    env = np.sin(np.pi * np.clip(t / (length + 0.25), 0.0, 1.0)) ** 1.5
    return swept_band(rng.standard_normal(n), centre, 1.2) * env


def swell(length: float, peak_at: float) -> np.ndarray:
    """Pink noise through a band-pass rising 300 Hz to 2.5 kHz, cresting at `peak_at` and gone by `length`."""
    n = int(round(length * SR))
    t = np.arange(n) / SR
    rise = np.clip(t / peak_at, 0.0, 1.0)
    centre = 300.0 * (2500.0 / 300.0) ** rise
    fall = np.clip((t - peak_at) / (length - peak_at), 0.0, 1.0)
    env = np.where(t < peak_at, rise**2.2, np.cos(fall * np.pi / 2.0) ** 2)
    return swept_band(pink(n), centre, 1.0) * env


def resolve_chord(length: float = 1.5) -> np.ndarray:
    notes = (A3, CS4, E4, A4)
    t = t_axis(length + 0.6)
    env = np.minimum(1.0, t / 0.03) * np.where(t < length, np.exp(-t / (length * 0.9)), np.exp(-length / (length * 0.9)) * np.exp(-(t - length) / 0.12))
    return sum(np.sin(2 * np.pi * f * t) + 0.12 * np.sin(4 * np.pi * f * t) for f in notes) * env


def bed(length: float) -> np.ndarray:
    """Stereo sine pad on A2 and E3. Harmonics pass a lowpass whose cutoff sweeps at 0.05 Hz."""
    t = t_axis(length)
    cutoff = 520.0 + 300.0 * np.sin(2 * np.pi * 0.05 * t - np.pi / 2.0)
    out = np.zeros((t.size, 2))
    for base, weight in ((A2, 1.0), (E3, 0.7)):
        for side, cents in ((0, -4.0), (1, 4.0)):
            f0 = base * 2 ** (cents / 1200.0)
            for h in range(1, 7):
                gain = weight / h / np.sqrt(1.0 + (f0 * h / cutoff) ** 4)
                out[:, side] += gain * np.sin(2 * np.pi * f0 * h * t + rng.uniform(0, 2 * np.pi))
    fade_in = np.minimum(1.0, t / 2.5)
    return out * fade_in[:, None]


def build(shots: dict) -> tuple[np.ndarray, int]:
    mix = np.zeros((N, 2))
    fade_out: tuple[float, float] | None = None
    count = 0
    # Glass cues pan across the sawtooth's x axis, so they read its length from the data.
    series_path = MEDIA.parent / "site" / "public" / "benchmarks" / "2026-09-28" / "sawtooth-series.json"
    series_len = len(json.loads(series_path.read_text())["without_proxy"])
    for shot in shots["shots"]:
        for cue in shot["cues"]:
            kind, at = cue["type"], float(cue["t"])
            count += 1
            if kind == "tick":
                accent = bool(cue.get("accent"))
                place(mix, click(), at, -17.0 if accent else -20.0, float(rng.uniform(-0.15, 0.15)))
            elif kind == "key":
                place(mix, click(0.002), at, -26.0, float(rng.uniform(-0.1, 0.1)))
            elif kind == "swell":
                place(mix, swell(float(cue["end"]) - at, float(cue["peak"]) - at), at, -21.0)
            elif kind == "bed":
                pad = bed(LENGTH_S - at)
                pad /= float(np.max(np.abs(pad)))
                start = int(round(at * SR))
                mix[start:] += pad * db(-28.0)
            elif kind == "place":
                place(mix, decaying_sine(E4, 0.4), at, -20.0)
            elif kind == "chime":
                place(mix, fm_bell(A5, 1.2), at, -19.0)
            elif kind == "whoosh":
                place(mix, whoosh_down(), at, -22.0)
            elif kind == "glass":
                pan = -0.6 + 1.2 * float(cue["request"]) / (series_len - 1)
                place(mix, decaying_sine(1800.0, 0.15), at, -21.0, pan)
            elif kind == "twonote-rise":
                place(mix, soft_note(E4, 1.2, 0.35), at, -20.0, -0.1)
                place(mix, soft_note(A4, 1.6, 0.5), at + 0.28, -20.0, 0.1)
            elif kind == "twonote-resolve":
                place(mix, soft_note(E4, 1.4, 0.4), at, -20.0, -0.1)
                place(mix, soft_note(A4, 2.4, 0.9), at + 0.36, -20.0, 0.1)
                place(mix, soft_note(A3, 2.4, 0.9), at + 0.36, -28.0)
            elif kind == "thud":
                place(mix, thud(), at, -16.0)
            elif kind == "resolve":
                place(mix, resolve_chord(1.5), at, -20.0)
            elif kind == "fadeout":
                fade_out = (at, float(cue["end"]))
            else:
                raise SystemExit(f"score.py: unknown cue type {kind!r} at {at} s in {shot['id']}")

    t = np.arange(N) / SR
    master = np.minimum(1.0, t / 1.0)
    a, b = fade_out if fade_out is not None else (74.0, 75.0)
    master *= np.clip((b - t) / (b - a), 0.0, 1.0) ** 2
    mix *= master[:, None]
    mix = limit(mix, -20.5)
    mix[-1] = 0.0
    return mix, count


def limit(x: np.ndarray, ceiling_db: float, lookahead: float = 0.010) -> np.ndarray:
    """Look-ahead peak limiter, linked across channels.

    The quiet bed sets the integrated loudness, so without this the few loud events give a
    peak-to-loudness ratio above 14 dB, and assemble.ts's two-pass loudnorm at -16 LUFS and
    -1.5 dBTP could not stay linear. Gain falls over the look-ahead window before a peak and
    recovers over the same window after it, so the ticks and notes keep their shape.
    """
    ceiling = db(ceiling_db)
    level = np.maximum(np.max(np.abs(x), axis=1), 1e-12)
    raw = np.minimum(1.0, ceiling / level)
    half = int(round(lookahead * SR))
    held = ndimage.minimum_filter1d(raw, size=2 * half + 1, mode="nearest")
    gain = np.minimum(ndimage.uniform_filter1d(held, size=half + 1, mode="nearest"), raw)
    return x * gain[:, None]


def write_wav24(path: Path, x: np.ndarray) -> None:
    peak = float(np.max(np.abs(x)))
    if peak > db(-1.0):
        x = x * (db(-1.0) / peak)
    ints = np.round(np.clip(x, -1.0, 1.0) * (2**23 - 1)).astype("<i4")
    raw = ints.reshape(-1).view(np.uint8).reshape(-1, 4)[:, :3].tobytes()
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(3)
        w.setframerate(SR)
        w.writeframes(raw)


def main() -> None:
    shots = json.loads((MEDIA / "shots.json").read_text())
    mix, count = build(shots)
    out = MEDIA / "out" / "score.wav"
    write_wav24(out, mix)
    peak = 20 * np.log10(max(float(np.max(np.abs(mix))), 1e-9))
    with wave.open(str(out), "rb") as w:
        frames = w.getnframes()
    if frames != N:
        raise SystemExit(f"score.py: wrote {frames} frames, expected {N}")
    json.dump({"score": str(out.relative_to(MEDIA.parent)), "cues": count, "frames": frames, "seconds": frames / SR, "peakDbfs": round(peak, 2)}, sys.stdout)
    print()


if __name__ == "__main__":
    main()
