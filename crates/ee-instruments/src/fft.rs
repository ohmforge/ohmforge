//! Hand-rolled radix-2 FFT and the scope's magnitude-spectrum math channel
//! (`CP-UX-M2.3`).
//!
//! The oscilloscope gains an FFT channel: feed it a real time-domain [`Signal`]
//! and it returns a one-sided magnitude spectrum as a frequency-domain complex
//! [`Signal`], ready for the [`Spectrum`](crate::spectrum::Spectrum) view. The
//! transform is a dependency-free, iterative radix-2 Cooley–Tukey FFT built on
//! `ee_core`'s hand-rolled [`Complex`] — no `rustfft`, no `num-complex`.
//!
//! Radix-2 needs a power-of-two length, so the real input is windowed and
//! zero-padded up to the next power of two. That keeps the public
//! [`magnitude_spectrum`] total: short, flat, or non-power-of-two inputs are
//! handled gracefully rather than panicking.

use core::f64::consts::TAU;
use ee_core::signal::{Complex, Domain, Signal, SignalData};
use ee_core::units::Unit;

/// An analysis window applied before the transform to trade main-lobe width for
/// side-lobe leakage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// No window (a boxcar). Cleanest for signals whose period divides the
    /// record exactly — an integer-cycle tone lands in a single bin.
    Rectangular,
    /// A Hann (raised-cosine) window. Suppresses spectral leakage for tones that
    /// do not align to a bin, at the cost of a wider main lobe.
    Hann,
}

impl Window {
    /// The window weight for sample `i` of `n` (uniform 1.0 for rectangular).
    fn weight(self, i: usize, n: usize) -> f64 {
        match self {
            Window::Rectangular => 1.0,
            Window::Hann if n <= 1 => 1.0,
            Window::Hann => 0.5 * (1.0 - (TAU * i as f64 / (n - 1) as f64).cos()),
        }
    }
}

/// In-place iterative radix-2 Cooley–Tukey FFT.
///
/// `a.len()` **must** be a power of two (0 and 1 are returned unchanged); the
/// caller zero-pads to satisfy that — see [`magnitude_spectrum`]. Computes the
/// unnormalized forward transform `A[k] = Σ a[t]·exp(-2πi·kt/N)`.
pub fn fft(a: &mut [Complex]) {
    let n = a.len();
    if n <= 1 {
        return;
    }
    debug_assert!(n.is_power_of_two(), "fft length must be a power of two");

    // Decimation-in-time bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            a.swap(i, j);
        }
    }

    // Butterflies, stage by stage, with incrementally advanced twiddles.
    let mut len = 2;
    while len <= n {
        let ang = -TAU / len as f64;
        let wlen = Complex::new(ang.cos(), ang.sin());
        let half = len / 2;
        let mut base = 0;
        while base < n {
            let mut w = Complex::ONE;
            for k in 0..half {
                let u = a[base + k];
                let v = a[base + k + half] * w;
                a[base + k] = u + v;
                a[base + k + half] = u - v;
                w = w * wlen;
            }
            base += len;
        }
        len <<= 1;
    }
}

/// Compute the one-sided magnitude spectrum of a real time-domain [`Signal`].
///
/// The samples are windowed, zero-padded to the next power of two, and
/// transformed. The result is a `Domain::Freq` complex `Signal` carrying bins
/// `0..=N/2`, with `x` the bin frequencies in hertz (derived from the mean
/// sample spacing). `Complex::magnitude_db` / `phase_deg` then read straight
/// off it. A non-time-domain, complex, or shorter-than-two-sample input yields
/// an empty frequency-domain signal rather than a panic.
pub fn magnitude_spectrum(sig: &Signal, window: Window) -> Signal {
    let name = format!("FFT[{}]", sig.name);
    let empty = || Signal::freq(name.clone(), Unit::None, Vec::new(), Vec::new());

    let y = match (sig.domain, &sig.y) {
        (Domain::Time, SignalData::Real(y)) if y.len() >= 2 => y,
        _ => return empty(),
    };
    let n0 = y.len();

    // Sample period from the average spacing of the time axis (uniform grid
    // assumed); fall back to 1 s so a malformed axis still yields a usable grid.
    let dt = if sig.x.len() >= 2 {
        let span = sig.x[sig.x.len() - 1] - sig.x[0];
        let d = span / (sig.x.len() - 1) as f64;
        if d.is_finite() && d > 0.0 {
            d
        } else {
            1.0
        }
    } else {
        1.0
    };
    let fs = 1.0 / dt;

    let n = n0.next_power_of_two();
    let mut buf = vec![Complex::ZERO; n];
    for (i, slot) in buf.iter_mut().enumerate().take(n0) {
        *slot = Complex::new(y[i] * window.weight(i, n0), 0.0);
    }
    fft(&mut buf);

    // One-sided: DC through Nyquist.
    let half = n / 2;
    let freqs: Vec<f64> = (0..=half).map(|k| k as f64 * fs / n as f64).collect();
    let spec: Vec<Complex> = buf[0..=half].to_vec();
    Signal::freq(name, sig.unit, freqs, spec)
}

/// The frequency (Hz) and magnitude (dB) of the strongest spectral bin.
///
/// `None` for an empty or non-complex spectrum. Handy as a peak-detector readout
/// and for testing that a tone lands where it should.
pub fn peak_bin(spectrum: &Signal) -> Option<(f64, f64)> {
    let c = spectrum.complex()?;
    let (idx, _) = c
        .iter()
        .enumerate()
        .filter(|(_, z)| z.magnitude().is_finite())
        .max_by(|(_, a), (_, b)| {
            a.magnitude()
                .partial_cmp(&b.magnitude())
                .unwrap_or(core::cmp::Ordering::Equal)
        })?;
    Some((spectrum.x[idx], c[idx].magnitude_db()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f64::consts::TAU;

    /// A direct O(N²) DFT — the reference the fast transform must match.
    fn dft(x: &[Complex]) -> Vec<Complex> {
        let n = x.len();
        (0..n)
            .map(|k| {
                let mut s = Complex::ZERO;
                for (t, &xt) in x.iter().enumerate() {
                    let ang = -TAU * (k * t) as f64 / n as f64;
                    s = s + xt * Complex::new(ang.cos(), ang.sin());
                }
                s
            })
            .collect()
    }

    #[test]
    fn fft_matches_direct_dft() {
        // A deterministic non-trivial signal at N = 16.
        let n = 16;
        let input: Vec<Complex> = (0..n)
            .map(|i| {
                let t = i as f64;
                Complex::new((0.7 * t).sin() + 0.3 * t, (1.3 * t).cos())
            })
            .collect();
        let reference = dft(&input);
        let mut fast = input.clone();
        fft(&mut fast);
        for (k, (f, r)) in fast.iter().zip(&reference).enumerate() {
            assert!(
                (f.re - r.re).abs() < 1e-9 && (f.im - r.im).abs() < 1e-9,
                "bin {k}: fft {f:?} vs dft {r:?}"
            );
        }
    }

    fn sine_signal(f0: f64, fs: f64, n: usize) -> Signal {
        let dt = 1.0 / fs;
        let x: Vec<f64> = (0..n).map(|i| i as f64 * dt).collect();
        let y: Vec<f64> = (0..n).map(|i| (TAU * f0 * i as f64 * dt).sin()).collect();
        Signal::time("v(in)", Unit::Volt, x, y)
    }

    #[test]
    fn pure_sine_has_one_dominant_bin_at_its_frequency() {
        // 8 Hz tone, 64 Hz sample rate, 64 samples → exactly 8 whole cycles, so
        // the energy lands in a single bin (k = f0·N/fs = 8) with no leakage.
        let (f0, fs, n) = (8.0, 64.0, 64);
        let spec = magnitude_spectrum(&sine_signal(f0, fs, n), Window::Rectangular);
        // Bin spacing is fs/N = 1 Hz, so bin index == frequency in Hz here.
        let (pf, _pdb) = peak_bin(&spec).expect("a peak");
        assert!(
            (pf - f0).abs() < fs / n as f64,
            "peak at {pf} Hz, want {f0}"
        );

        // It really is a single dominant bin: every other bin is far smaller.
        let c = spec.complex().unwrap();
        let peak = c.iter().map(|z| z.magnitude()).fold(0.0, f64::max);
        let others: f64 = c
            .iter()
            .map(|z| z.magnitude())
            .filter(|&m| m < peak)
            .fold(0.0, f64::max);
        assert!(
            others < 1e-6 * peak,
            "a second bin rivals the peak: {others}"
        );
    }

    #[test]
    fn two_tone_input_shows_two_peaks() {
        let (fs, n) = (64.0, 64);
        let (f0, f1) = (8.0, 20.0);
        let dt = 1.0 / fs;
        let x: Vec<f64> = (0..n).map(|i| i as f64 * dt).collect();
        let y: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 * dt;
                (TAU * f0 * t).sin() + 0.5 * (TAU * f1 * t).sin()
            })
            .collect();
        let spec = magnitude_spectrum(&Signal::time("v", Unit::Volt, x, y), Window::Rectangular);
        let c = spec.complex().unwrap();

        // The bins that carry non-negligible energy are exactly 8 Hz and 20 Hz.
        let peak = c.iter().map(|z| z.magnitude()).fold(0.0, f64::max);
        let strong: Vec<f64> = spec
            .x
            .iter()
            .zip(c)
            .filter(|(_, z)| z.magnitude() > 1e-6 * peak)
            .map(|(&f, _)| f)
            .collect();
        assert_eq!(strong, vec![f0, f1], "expected two tones, got {strong:?}");
    }

    #[test]
    fn non_power_of_two_is_zero_padded_without_panic() {
        // 10 samples → padded to 16; must not panic and must return a spectrum.
        let spec = magnitude_spectrum(&sine_signal(3.0, 20.0, 10), Window::Hann);
        assert!(!spec.is_empty());
        // 16-point transform, one-sided → 9 bins (0..=8).
        assert_eq!(spec.len(), 9);
    }

    #[test]
    fn short_flat_and_complex_inputs_do_not_panic() {
        // Single sample: too short → empty.
        let one = Signal::time("v", Unit::Volt, vec![0.0], vec![1.0]);
        assert!(magnitude_spectrum(&one, Window::Rectangular).is_empty());

        // Flat (DC) signal: all energy in bin 0, no panic.
        let x: Vec<f64> = (0..8).map(|i| i as f64).collect();
        let flat = Signal::time("v", Unit::Volt, x, vec![2.5; 8]);
        let spec = magnitude_spectrum(&flat, Window::Rectangular);
        let (pf, _) = peak_bin(&spec).unwrap();
        assert_eq!(pf, 0.0, "a flat signal peaks at DC");

        // A frequency-domain (complex) input is rejected gracefully.
        let cplx = Signal::freq("h", Unit::None, vec![1.0], vec![Complex::ONE]);
        assert!(magnitude_spectrum(&cplx, Window::Rectangular).is_empty());
    }

    #[test]
    fn fft_of_length_one_and_zero_are_noops() {
        let mut a = [Complex::new(3.0, -1.0)];
        fft(&mut a);
        assert_eq!(a[0], Complex::new(3.0, -1.0));
        let mut e: [Complex; 0] = [];
        fft(&mut e); // must not panic
    }
}
