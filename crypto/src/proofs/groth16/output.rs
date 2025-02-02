use std::str::FromStr;

use ark_r1cs_std::uint8::UInt8;
use decaf377::{
    r1cs::{ElementVar, FqVar},
    Bls12_377, Fq, Fr,
};
use decaf377::{Element, FieldExt};
use decaf377_fmd as fmd;
use decaf377_ka as ka;

use ark_ff::{PrimeField, ToConstraintField};
use ark_groth16::{Groth16, Proof, ProvingKey, VerifyingKey};
use ark_r1cs_std::prelude::*;
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef};
use ark_snark::SNARK;
use rand::{CryptoRng, Rng};
use rand_core::OsRng;

use crate::proofs::groth16::{gadgets, ParameterSetup};
use crate::{balance, keys::Diversifier, note, Address, Note, Rseed, Value};

// Public:
// * vcm (value commitment)
// * ncm (note commitment)
//
// Witnesses:
// * g_d (point)
// * pk_d (point)
// * v (u64 value plus asset ID (scalar))
// * vblind (Fr)
// * nblind (Fq)
#[derive(Clone, Debug)]
pub struct OutputCircuit {
    // Witnesses
    /// The note being created.
    note: Note,
    /// The blinding factor used for generating the balance commitment.
    v_blinding: Fr,

    // Public inputs
    /// balance commitment of the new note,
    pub balance_commitment: balance::Commitment,
    /// note commitment of the new note,
    pub note_commitment: note::Commitment,
}

impl ConstraintSynthesizer<Fq> for OutputCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fq>) -> ark_relations::r1cs::Result<()> {
        // Witnesses
        let note_blinding_var =
            FqVar::new_witness(cs.clone(), || Ok(self.note.note_blinding().clone()))?;
        let value_amount_var =
            FqVar::new_witness(cs.clone(), || Ok(Fq::from(self.note.value().amount)))?;
        let value_asset_id_var =
            FqVar::new_witness(cs.clone(), || Ok(self.note.value().asset_id.0))?;
        let diversified_generator_var: ElementVar =
            AllocVar::<Element, Fq>::new_witness(cs.clone(), || {
                Ok(self.note.diversified_generator().clone())
            })?;
        let transmission_key_s_var =
            FqVar::new_witness(cs.clone(), || Ok(self.note.transmission_key_s().clone()))?;
        let clue_key_var = FqVar::new_witness(cs.clone(), || {
            Ok(Fq::from_le_bytes_mod_order(&self.note.clue_key().0[..]))
        })?;
        let v_blinding_arr: [u8; 32] = self.v_blinding.to_bytes();
        let v_blinding_vars = UInt8::new_witness_vec(cs.clone(), &v_blinding_arr)?;
        let value_amount_arr = self.note.value().amount.to_le_bytes();
        let value_vars = UInt8::new_witness_vec(cs.clone(), &value_amount_arr)?;

        // Public inputs
        let note_commitment_var = FqVar::new_input(cs.clone(), || Ok(self.note_commitment.0))?;
        let balance_commitment_var =
            ElementVar::new_input(cs.clone(), || Ok(self.balance_commitment.0))?;

        gadgets::diversified_basepoint_not_identity(
            cs.clone(),
            &Boolean::TRUE,
            diversified_generator_var.clone(),
        )?;
        gadgets::value_commitment_integrity(
            cs.clone(),
            &Boolean::TRUE,
            value_vars,
            value_asset_id_var.clone(),
            v_blinding_vars,
            balance_commitment_var,
        )?;
        gadgets::note_commitment_integrity(
            cs,
            &Boolean::TRUE,
            note_blinding_var,
            value_amount_var,
            value_asset_id_var,
            diversified_generator_var,
            transmission_key_s_var,
            clue_key_var,
            note_commitment_var,
        )?;

        Ok(())
    }
}

impl ParameterSetup for OutputCircuit {
    fn generate_test_parameters() -> (ProvingKey<Bls12_377>, VerifyingKey<Bls12_377>) {
        let diversifier_bytes = [1u8; 16];
        let pk_d_bytes = [1u8; 32];
        let clue_key_bytes = [1; 32];
        let diversifier = Diversifier(diversifier_bytes);
        let address = Address::from_components(
            diversifier,
            ka::Public(pk_d_bytes),
            fmd::ClueKey(clue_key_bytes),
        )
        .expect("generated 1 address");
        let note = Note::from_parts(
            address,
            Value::from_str("1upenumbra").expect("valid value"),
            Rseed([1u8; 32]),
        )
        .expect("can make a note");
        let v_blinding = Fr::from(1);
        let circuit = OutputCircuit {
            note: note.clone(),
            note_commitment: note.commit(),
            v_blinding,
            balance_commitment: balance::Commitment(decaf377::basepoint()),
        };
        let (pk, vk) = Groth16::circuit_specific_setup(circuit, &mut OsRng)
            .expect("can perform circuit specific setup");
        (pk, vk)
    }
}

pub struct OutputProof(Proof<Bls12_377>);

impl OutputProof {
    #![allow(clippy::too_many_arguments)]
    pub fn prove<R: CryptoRng + Rng>(
        rng: &mut R,
        pk: &ProvingKey<Bls12_377>,
        note: Note,
        v_blinding: Fr,
        balance_commitment: balance::Commitment,
        note_commitment: note::Commitment,
    ) -> anyhow::Result<Self> {
        let circuit = OutputCircuit {
            note,
            note_commitment,
            v_blinding,
            balance_commitment,
        };
        let proof = Groth16::prove(pk, circuit, rng).map_err(|err| anyhow::anyhow!(err))?;
        Ok(Self(proof))
    }

    /// Called to verify the proof using the provided public inputs.
    ///
    /// The public inputs are:
    /// * balance commitment of the new note,
    /// * note commitment of the new note,
    pub fn verify(
        &self,
        vk: &VerifyingKey<Bls12_377>,
        balance_commitment: balance::Commitment,
        note_commitment: note::Commitment,
    ) -> anyhow::Result<()> {
        let processed_pvk = Groth16::process_vk(vk).map_err(|err| anyhow::anyhow!(err))?;
        let mut public_inputs = Vec::new();
        public_inputs.extend(note_commitment.0.to_field_elements().unwrap());
        public_inputs.extend(balance_commitment.0.to_field_elements().unwrap());

        let proof_result =
            Groth16::verify_with_processed_vk(&processed_pvk, public_inputs.as_slice(), &self.0)
                .map_err(|err| anyhow::anyhow!(err))?;
        proof_result
            .then_some(())
            .ok_or_else(|| anyhow::anyhow!("proof did not verify"))
    }
}
