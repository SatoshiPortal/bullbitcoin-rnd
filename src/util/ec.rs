#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use elements::secp256k1_zkp::{Generator, RangeProof, SecretKey, Tag, Tweak};

    use bitcoin::hex::FromHex;
    use elements::secp256k1_zkp::{PedersenCommitment, Secp256k1 as ZKSecp256k1};

    #[macros::test_all]
    fn test_ct_primitives() {
        // 1. Test Pedersen commitment - https://docs.rs/secp256k1-zkp/0.9.2/secp256k1_zkp/struct.PedersenCommitment.html
        let secp = ZKSecp256k1::new();
        let value = 50_000;

        let blinding_factor_n = Tweak::from_inner(
            <[u8; 32]>::from_hex(
                "dfad9ad4ab3d475c487858cbab210e893a6dfffa814851e4692f91f8a0818a3a",
            )
            .unwrap(),
        );
        let additional_generator =
            Generator::new_blinded(&secp, Tag::default(), blinding_factor_n.unwrap());
        let blinding_factor_r = Tweak::from_inner(
            <[u8; 32]>::from_hex(
                "aa24825c14e0bf855e9bc3c877eadee934a9e870ea69fe6678650c4ab99e2d25",
            )
            .unwrap(),
        );
        // Pedersen commitment of form X = r*G + v*H
        //     where X is the Pedersen commitment
        //     r is the blinding_factor
        //     v is value to commit
        //     H is additional_generator and G is secp256k1's fixed generator point
        let pc = PedersenCommitment::new(
            &secp,
            value,
            blinding_factor_r.unwrap(),
            additional_generator,
        );
        let expected_pc = "09c02e309a8ac06d644aab0c8b52dd4318825340d7a33336a0b496a21cbea56229";
        assert_eq!(expected_pc, pc.to_string());

        // 2. Create and verify range proofs - https://docs.rs/secp256k1-zkp/0.9.2/secp256k1_zkp/struct.RangeProof.html
        let min_value: u64 = 0;
        let secret_key =
            SecretKey::from_str("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        let exp: i32 = 0;
        let min_bits: u8 = 36;
        let message: &[u8] = &[];
        let additional_commitment: &[u8] = &[];

        let range_proof = RangeProof::new(
            &secp,
            min_value, // constructs a proof where the verifer can tell the minimum value is at least the specified amount.
            pc,        // the commitment being proved.
            value,     // Actual value of the commitment.
            blinding_factor_r.unwrap(), // 32-byte blinding factor used by value.
            message, // pointer to a byte array of data to be embedded in the rangeproof that can be recovered by rewinding the proof
            additional_commitment, // additional data to be covered in rangeproof signature
            secret_key, // 32-byte secret nonce used to initialize the proof (value can be reverse-engineered out of the proof if this secret is known.)
            exp, // Base-10 exponent. Digits below above will be made public, but the proof will be made smaller. Allowed range is -1 to 18. (-1 is a special case that makes the value public. 0 is the most private.)
            min_bits, // Number of bits of the value to keep private. (0 = auto/minimal, - 64).
            additional_generator, // additional generator 'h'
        )
        .unwrap();
        let range = range_proof
            .verify(&secp, pc, additional_commitment, additional_generator)
            .unwrap();
        assert_eq!(range.start, 0);
        assert_eq!(range.end, u64::pow(2, min_bits as u32));

        let (opening, _range) = range_proof
            .rewind(
                &secp,
                pc,
                secret_key,
                additional_commitment,
                additional_generator,
            )
            .unwrap();
        assert_eq!(opening.value, value);
        assert_eq!(opening.blinding_factor, blinding_factor_r.unwrap());
    }
}
