use {
    criterion::{
        AxisScale, BatchSize, BenchmarkId, Criterion, PlotConfiguration, criterion_group,
        criterion_main,
    },
    dango_crypto::{
        keccak256, secp256k1_pubkey_recover, secp256k1_verify, secp256r1_verify, sha2_256,
    },
    p256::{ecdsa::signature::hazmat::PrehashSigner, elliptic_curve::Generate},
    std::{hint::black_box, time::Duration},
};

struct Settings {
    warmup_time: Duration,
    measurement_time: Duration,
}

const HASH_SETTINGS: Settings = Settings {
    warmup_time: Duration::from_millis(500),
    measurement_time: Duration::from_millis(1_500),
};

const CRYPTO_SETTINGS: Settings = Settings {
    warmup_time: Duration::from_millis(2_000),
    measurement_time: Duration::from_millis(5_000),
};

/// Lengths of messages for benchmarking hashers.
///
/// In Dango contracts, the largest data that may need to hashed are transactions
/// that contain `Message::Upload` messages. Contract size are usually a few
/// hundred kilobytes, so here we choose messages of up to 1 MiB.
const HASH_MSG_LENS: [usize; 5] = [200_000, 400_000, 600_000, 800_000, 1_000_000];

/// Messages to be signed for benchmarking verifiers.
///
/// This length doesn't matter, because verifiers only concern hashes, so we
/// just pick a small number.
const SIGN_MSG_LEN: usize = 10;

fn generate_random_msg(i: usize) -> Vec<u8> {
    let mut vec = vec![0; i];
    getrandom::fill(&mut vec).unwrap();
    vec
}

fn bench_hashers(c: &mut Criterion) {
    let mut group = c.benchmark_group("hashers");

    group.plot_config(PlotConfiguration::default().summary_scale(AxisScale::Linear));
    group.warm_up_time(HASH_SETTINGS.warmup_time);
    group.measurement_time(HASH_SETTINGS.measurement_time);

    for size in HASH_MSG_LENS {
        group.bench_with_input(BenchmarkId::new("sha2_256", size), &size, |b, size| {
            b.iter_batched(
                || generate_random_msg(*size),
                |data| sha2_256(black_box(&data)),
                BatchSize::SmallInput,
            );
        });

        group.bench_with_input(BenchmarkId::new("keccak256", size), &size, |b, size| {
            b.iter_batched(
                || generate_random_msg(*size),
                |data| keccak256(black_box(&data)),
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_verifiers(c: &mut Criterion) {
    let mut group = c.benchmark_group("verifiers");

    group.plot_config(PlotConfiguration::default().summary_scale(AxisScale::Linear));
    group.warm_up_time(CRYPTO_SETTINGS.warmup_time);
    group.measurement_time(CRYPTO_SETTINGS.measurement_time);

    group.bench_function("secp256r1_verify", |b| {
        b.iter_batched(
            || {
                let msg = generate_random_msg(SIGN_MSG_LEN);
                let msg_hash = sha2_256(&msg);
                let sk = p256::ecdsa::SigningKey::generate();
                let vk = p256::ecdsa::VerifyingKey::from(&sk);
                let sig: p256::ecdsa::Signature = sk.sign_prehash(&msg_hash).unwrap();

                (
                    msg_hash.to_vec(),
                    sig.to_bytes().to_vec(),
                    vk.to_sec1_bytes().to_vec(),
                )
            },
            |(msg_hash, sig, vk)| {
                assert!(secp256r1_verify(&msg_hash, &sig, &vk).is_ok());
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("secp256k1_verify", |b| {
        b.iter_batched(
            || {
                let msg = generate_random_msg(SIGN_MSG_LEN);
                let msg_hash = sha2_256(&msg);
                let sk = k256::ecdsa::SigningKey::generate();
                let vk = k256::ecdsa::VerifyingKey::from(&sk);
                let sig: k256::ecdsa::Signature = sk.sign_prehash(&msg_hash).unwrap();

                (
                    msg_hash.to_vec(),
                    sig.to_bytes().to_vec(),
                    vk.to_sec1_bytes().to_vec(),
                )
            },
            |(msg_hash, sig, vk)| {
                assert!(secp256k1_verify(&msg_hash, &sig, &vk).is_ok());
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("secp256k1_pubkey_recover", |b| {
        b.iter_batched(
            || {
                let msg = generate_random_msg(SIGN_MSG_LEN);
                let msg_hash = sha2_256(&msg);
                let sk = k256::ecdsa::SigningKey::generate();
                let vk = k256::ecdsa::VerifyingKey::from(&sk);
                let (sig, recovery_id) = sk.sign_prehash_recoverable(&msg_hash);

                (
                    msg_hash.to_vec(),
                    sig.to_bytes().to_vec(),
                    vk.to_sec1_bytes().to_vec(),
                    recovery_id.to_byte(),
                    true,
                )
            },
            |(msg_hash, sig, vk, recovery_id, compressed)| {
                assert_eq!(
                    secp256k1_pubkey_recover(&msg_hash, &sig, recovery_id, compressed).unwrap(),
                    vk
                );
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_hashers, bench_verifiers);

criterion_main!(benches);
