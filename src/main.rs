use cubeb::StereoFrame;
use std::f32::consts::PI;
use std::sync::Arc;

use audio_clock::audio_clock;
use monome::{Monome, MonomeEvent};
use std::{thread, time};
use crossbeam::queue::ArrayQueue;

const SAMPLE_FREQUENCY: u32 = 48_000;
const STREAM_FORMAT: cubeb::SampleFormat = cubeb::SampleFormat::Float32NE;

type Frame = StereoFrame<f32>;

#[derive(Debug)]
struct ADSR {
    start_time: usize,
    attack: usize,
    decay: usize,
    hold: usize,
    release: usize,
    sample_rate: f32,
    sustain: f32,
}

impl ADSR {
    pub fn new(sample_rate: f32) -> ADSR {
        ADSR {
            start_time: 0,
            attack: 0,
            decay: 0,
            hold: 0,
            release: (sample_rate / 100.) as usize,
            sample_rate,
            sustain: 1.0,
        }
    }
    pub fn render(&mut self, t: usize) -> f32 {
        // shaping: [0,1] -> [0,1]
        let f = |t: f32| -> f32 {
            return t * t;
        };
        if t > self.start_time + self.duration() || t < self.start_time {
            return 0.0;
        }
        let mut t = t - self.start_time;
        if t < self.attack {
            return f(t as f32 / self.attack as f32);
        }
        t -= self.attack;
        if t < self.decay {
            return 1.0 - (1.0 - self.sustain) * f((t as f32) / (self.decay) as f32);
        }
        t -= self.decay;
        if t < self.hold {
            return self.sustain;
        }
        t -= self.hold;
        return self.sustain - self.sustain * f(t as f32 / (self.release) as f32);
    }
    pub fn set_attack(&mut self, attack: f32) {
        self.attack = self.s2f(attack);
    }
    pub fn set_decay(&mut self, decay: f32) {
        self.decay = self.s2f(decay);
    }
    pub fn set_hold(&mut self, hold: f32) {
        self.hold = self.s2f(hold);
    }
    pub fn set_sustain(&mut self, sustain: f32) {
        self.sustain = sustain;
    }
    pub fn set_release(&mut self, release: f32) {
        self.release = self.s2f(release);
    }
    pub fn trigger(&mut self, time: usize) {
        self.start_time = time;
    }
    fn duration(&self) -> usize {
        self.attack + self.decay + self.hold + self.release
    }
    fn f2s(&self, t: usize) -> f32 {
        (t as f32) / self.sample_rate
    }
    fn s2f(&self, s: f32) -> usize {
        (s * self.sample_rate) as usize
    }
}

struct Param {
    v0: f32,
    v1: f32,
    counter: isize,
    smoothing: isize,
    sample_rate: f32,
}

impl Param {
    fn new(sample_rate: f32, v: f32) -> Param {
        Param {
            v0: v,
            v1: v,
            counter: 1000,
            smoothing: 1000,
            sample_rate,
        }
    }
    fn value(&mut self) -> f32 {
        // if self.counter == self.smoothing {
        //     self.v0 = self.v1;
        //     return self.v0;
        // }
        // self.counter += 1;
        // let v = self.v1 + (self.v0 - self.v1) * (((-(self.counter - self.smoothing) as f32 / self.sample_rate) / 0.01)).exp();
        // println!("smooting {} ({} to {}) ({} to {})", v, self.v0, self.v1, self.counter, self.smoothing);
        return self.v0;
    }
    fn set_value(&mut self, v: f32) {
        //println!("set_value to {}", v);
        //self.counter = 0;
        //self.v1 = v;
        self.v0 = v;
    }
    fn set_value_no_smooth(&mut self, v: f32) {
        self.counter = self.smoothing;
        self.v0 = v;
        self.v1 = v;
    }
}

struct Oscillator {
    phase: f32,
    sample_rate: f32,
    frequency: Param,
    detune: Param,
}

impl Oscillator {
    fn new(sample_rate: f32) -> Oscillator  {
        Oscillator {
            phase: 0.,
            frequency: Param::new(sample_rate, 440.),
            sample_rate,
            detune: Param::new(sample_rate, 0.),
        }
    }
    fn render(&mut self, _t: usize) -> f32 {
        let final_frequency = self.frequency.value() + (self.detune.value() / 1200.).exp2();
        let period = self.sample_rate / final_frequency;
        let phase_increment = 2. * PI / period;
        let rv = self.phase.sin();
        self.phase += phase_increment;

        if self.phase > 2. * PI {
            self.phase -= 2. * PI;
        }
        if self.phase != self.phase {
            self.phase = 0.;
        }
        return rv;
    }
    fn set_frequency(&mut self, frequency: f32) {
        self.frequency.set_value(frequency);
    }
    fn set_frequency_no_smooth(&mut self, frequency: f32) {
        self.frequency.set_value_no_smooth(frequency);
    }
    // [-1200, 1200]
    fn set_detune(&mut self, detune: f32) {
        self.detune.set_value(detune);
    }
    fn set_phase(&mut self, phase: f32) {
        // modulo 2 * PI ?
        self.phase = phase;
    }
}

#[derive(Debug, Copy, Clone)]
enum Parameters {
    CarrierFreq(f32),
    ModulationFreq(f32),
    Attack(f32),
    Release(f32),
}

fn main() {
    let ctx = cubeb::init("redh").expect("Failed to create cubeb context");

    let params = cubeb::StreamParamsBuilder::new()
        .format(STREAM_FORMAT)
        .rate(SAMPLE_FREQUENCY)
        .channels(2)
        .layout(cubeb::ChannelLayout::STEREO)
        .take();

    let (mut updater, consumer) = audio_clock(128., SAMPLE_FREQUENCY);
    let _consumer2 = consumer.clone();
    let mut env = ADSR::new(SAMPLE_FREQUENCY as f32);
    let mut osc = Oscillator::new(SAMPLE_FREQUENCY as f32);
    let mut osc_mod = Oscillator::new(SAMPLE_FREQUENCY as f32);

    let q = Arc::new(ArrayQueue::new(16));
    let q2 = q.clone();

    let mut builder = cubeb::StreamBuilder::<Frame>::new();
    builder
        .name("redh")
        .default_output(&params)
        .data_callback(move |_, output| {
            match q2.pop() {
                Ok(m) => {
                    match m {
                        Parameters::CarrierFreq(v) => {
                            osc.set_frequency(v);
                        }
                        Parameters::ModulationFreq(v) => {
                            osc_mod.set_frequency(v);
                        }
                        Parameters::Release(v) => {
                            env.set_release(v);
                        }
                        Parameters::Attack(v) => {
                            env.set_attack(v);
                        }
                    }
                }
                _ => { }
            }
            for f in output.iter_mut() {
                let t = consumer.raw_frames();
                if consumer.raw_frames() % 48000 == 0 {
                    env.trigger(t);
                }
                let m = osc_mod.render(t);
                osc.set_frequency_no_smooth((m + 1.0) * 100.);
                let g = env.render(t);
                let s = osc.render(t);
                f.l = g * s;
                f.r = f.l;
                updater.increment(1);
            }
            output.len() as isize
        })
        .state_callback(|state| {
            println!("stream {:?}", state);
        });

    let stream = builder.init(&ctx).expect("Failed to create cubeb stream");

    let mut monome = Monome::new("/prefix".to_string()).unwrap();

    stream.start().unwrap();

    for i in 0..4 {
        monome.ring_all(i, 0);
    }

    let mut freq = 110.;
    let mut mod_freq = 55.;
    let mut attack = 0.1;
    let mut release = 0.9;
    let mut led = [0.; 4];
    q.push(Parameters::CarrierFreq(freq)).unwrap();
    q.push(Parameters::ModulationFreq(mod_freq)).unwrap();
    q.push(Parameters::Attack(attack)).unwrap();
    q.push(Parameters::Release(release)).unwrap();

    loop {
        loop {
            let e = monome.poll();

            match e {
                Some(MonomeEvent::EncoderDelta { n, delta }) => {
                    let n = n as usize;
                    monome.ring_set(n, led[n] as u32, 0);
                    led[n] = led[n] + (delta as f32 / 10.);
                    if led[n] < 0. {
                        led[n] = 0.;
                    }
                    if led[n] > 64. {
                        led[n] = 64.;
                    }
                    monome.ring_set(n, led[n] as u32, 3);
                    match n {
                        0 => {
                            freq = led[0] * 10.;
                            q.push(Parameters::CarrierFreq(freq)).unwrap();
                        }
                        1 => {
                            mod_freq = led[1] * 10.;
                            q.push(Parameters::ModulationFreq(mod_freq)).unwrap();
                        }
                        2 => {
                            attack = led[2] / 64.;
                            q.push(Parameters::Attack(attack)).unwrap();
                        }
                        3 => {
                            release = led[3] / 64.;
                            q.push(Parameters::Release(release)).unwrap();
                        }
                        _ => {}
                    }
                }
                _ => {
                    break;

                }
            }
        }

        //println!("{} {} {} {}", freq, mod_freq, attack, release);

        let refresh = time::Duration::from_millis(10);
        thread::sleep(refresh);
    }
}


