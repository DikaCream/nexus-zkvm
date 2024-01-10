use std::{marker::PhantomData, num::NonZeroU64};

use ark_crypto_primitives::sponge::{
    constraints::{CryptographicSpongeVar, SpongeWithGadget},
    Absorb, CryptographicSponge,
};
use ark_ec::short_weierstrass::{Projective, SWCurveConfig};
use ark_ff::{AdditiveGroup, PrimeField};
use ark_r1cs_std::R1CSVar;
use ark_relations::r1cs::{ConstraintSystem, SynthesisMode};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};

use crate::{
    absorb::CryptographicSpongeExt,
    commitment::CommitmentScheme,
    multifold::{
        self,
        nimfs::{
            NIMFSProof, R1CSInstance, R1CSShape, R1CSWitness, RelaxedR1CSInstance,
            RelaxedR1CSWitness,
        },
    },
};

use super::{public_params, NovaConstraintSynthesizer, StepCircuit};

mod augmented;
use augmented::{
    NovaAugmentedCircuit, NovaAugmentedCircuitInput, NovaAugmentedCircuitNonBaseInput,
};

const LOG_TARGET: &str = "supernova::sequential";

#[doc(hidden)]
pub struct SetupParams<T>(PhantomData<T>);

impl<G1, G2, C1, C2, RO, SC> public_params::SetupParams<G1, G2, C1, C2, RO, SC>
    for SetupParams<(G1, G2, C1, C2, RO, SC)>
where
    G1: SWCurveConfig,
    G2: SWCurveConfig<BaseField = G1::ScalarField, ScalarField = G1::BaseField>,
    G1::BaseField: PrimeField + Absorb,
    G2::BaseField: PrimeField + Absorb,
    C1: CommitmentScheme<Projective<G1>, Commitment = Projective<G1>>,
    C2: CommitmentScheme<Projective<G2>, Commitment = Projective<G2>>,
    RO: SpongeWithGadget<G1::ScalarField> + Send + Sync,
    RO::Var: CryptographicSpongeVar<G1::ScalarField, RO, Parameters = RO::Config>,
    RO::Config: CanonicalSerialize + CanonicalDeserialize + Sync,
    SC: StepCircuit<G1::ScalarField>,
{
    fn setup(
        ro_config: <RO as CryptographicSponge>::Config,
        step_circuit: &SC,
        aux1: &C1::SetupAux,
        aux2: &C2::SetupAux,
    ) -> Result<public_params::PublicParams<G1, G2, C1, C2, RO, SC, Self>, multifold::Error> {
        let _span = tracing::debug_span!(target: LOG_TARGET, "setup").entered();

        let z_0 = vec![G1::ScalarField::ZERO; SC::ARITY];

        let cs = ConstraintSystem::new_ref();
        cs.set_mode(SynthesisMode::Setup);

        let input = NovaAugmentedCircuitInput::<G1, G2, C1, C2, RO>::Base {
            vk: G1::ScalarField::ZERO,
            z_0,
        };
        let circuit = NovaAugmentedCircuit::new(&ro_config, step_circuit, input);
        let _ = NovaConstraintSynthesizer::generate_constraints(circuit, cs.clone())?;

        cs.finalize();

        let shape = R1CSShape::from(cs);
        let shape_secondary = multifold::secondary::setup_shape::<G1, G2>()?;

        let pp = C1::setup(shape.num_vars.max(shape.num_constraints), aux1);
        let pp_secondary = C2::setup(
            shape_secondary
                .num_vars
                .max(shape_secondary.num_constraints),
            aux2,
        );

        let mut params = public_params::PublicParams {
            ro_config,
            shape,
            shape_secondary,
            pp,
            pp_secondary,
            digest: G1::ScalarField::ZERO,

            _step_circuit: PhantomData,
            _setup_params: PhantomData,
        };
        let digest = params.hash();
        params.digest = digest;

        tracing::debug!(
            target: LOG_TARGET,
            "public params setup done; augmented circuit: {}, secondary circuit: {}",
            params.shape,
            params.shape_secondary,
        );
        Ok(params)
    }
}

pub type PublicParams<G1, G2, C1, C2, RO, SC> =
    public_params::PublicParams<G1, G2, C1, C2, RO, SC, SetupParams<(G1, G2, C1, C2, RO, SC)>>;

pub struct IVCProof<'a, G1, G2, C1, C2, RO, SC>
where
    G1: SWCurveConfig,
    G2: SWCurveConfig,
    C1: CommitmentScheme<Projective<G1>>,
    C2: CommitmentScheme<Projective<G2>>,
    RO: CryptographicSponge + Send + Sync,
    RO::Config: CanonicalSerialize + CanonicalDeserialize + Sync,
    SC: StepCircuit<G1::ScalarField>,
{
    params: &'a PublicParams<G1, G2, C1, C2, RO, SC>,
    z_0: Vec<G1::ScalarField>,

    non_base: Option<IVCProofNonBase<G1, G2, C1, C2>>,
}

impl<G1, G2, C1, C2, RO, SC> Clone for IVCProof<'_, G1, G2, C1, C2, RO, SC>
where
    G1: SWCurveConfig,
    G2: SWCurveConfig,
    C1: CommitmentScheme<Projective<G1>>,
    C2: CommitmentScheme<Projective<G2>>,
    RO: CryptographicSponge + Send + Sync,
    RO::Config: CanonicalSerialize + CanonicalDeserialize + Sync,
    SC: StepCircuit<G1::ScalarField>,
{
    fn clone(&self) -> Self {
        Self {
            params: self.params,
            z_0: self.z_0.clone(),
            non_base: self.non_base.clone(),
        }
    }
}

struct IVCProofNonBase<G1, G2, C1, C2>
where
    G1: SWCurveConfig,
    G2: SWCurveConfig,
    C1: CommitmentScheme<Projective<G1>>,
    C2: CommitmentScheme<Projective<G2>>,
{
    U: RelaxedR1CSInstance<G1, C1>,
    W: RelaxedR1CSWitness<G1>,
    U_secondary: RelaxedR1CSInstance<G2, C2>,
    W_secondary: RelaxedR1CSWitness<G2>,

    u: R1CSInstance<G1, C1>,
    w: R1CSWitness<G1>,
    i: NonZeroU64,
    z_i: Vec<G1::ScalarField>,
}

impl<G1, G2, C1, C2> Clone for IVCProofNonBase<G1, G2, C1, C2>
where
    G1: SWCurveConfig,
    G2: SWCurveConfig,
    C1: CommitmentScheme<Projective<G1>>,
    C2: CommitmentScheme<Projective<G2>>,
{
    fn clone(&self) -> Self {
        Self {
            U: self.U.clone(),
            W: self.W.clone(),
            U_secondary: self.U_secondary.clone(),
            W_secondary: self.W_secondary.clone(),
            u: self.u.clone(),
            w: self.w.clone(),
            i: self.i,
            z_i: self.z_i.clone(),
        }
    }
}

impl<'a, G1, G2, C1, C2, RO, SC> IVCProof<'a, G1, G2, C1, C2, RO, SC>
where
    G1: SWCurveConfig,
    G2: SWCurveConfig<BaseField = G1::ScalarField, ScalarField = G1::BaseField>,
    G1::BaseField: PrimeField + Absorb,
    G2::BaseField: PrimeField + Absorb,
    C1: CommitmentScheme<Projective<G1>, Commitment = Projective<G1>>,
    C2: CommitmentScheme<Projective<G2>, Commitment = Projective<G2>>,
    RO: SpongeWithGadget<G1::ScalarField> + Send + Sync,
    RO::Var: CryptographicSpongeVar<G1::ScalarField, RO, Parameters = RO::Config>,
    RO::Config: CanonicalSerialize + CanonicalDeserialize + Sync,
    SC: StepCircuit<G1::ScalarField>,
{
    pub fn new(
        public_params: &'a PublicParams<G1, G2, C1, C2, RO, SC>,
        z_0: &[G1::ScalarField],
    ) -> Self {
        Self {
            params: public_params,
            z_0: z_0.to_owned(),

            non_base: None,
        }
    }

    pub fn z_i(&self) -> &[G1::ScalarField] {
        self.non_base
            .as_ref()
            .map(|r| &r.z_i[..])
            .unwrap_or(&self.z_0)
    }

    pub fn step_num(&self) -> u64 {
        self.non_base
            .as_ref()
            .map(|non_base| non_base.i.get())
            .unwrap_or(0)
    }

    pub fn prove_step(self, step_circuit: &SC) -> Result<Self, multifold::Error> {
        let _span = tracing::debug_span!(
            target: LOG_TARGET,
            "prove_step",
            step_num = %self.step_num(),
        )
        .entered();
        let IVCProof {
            params,
            z_0,
            non_base,
        } = self;

        let (i_next, input, U, W, U_secondary, W_secondary) = if let Some(non_base) = non_base {
            let IVCProofNonBase {
                U,
                W,
                U_secondary,
                W_secondary,
                u,
                w,
                i,
                z_i,
            } = non_base;

            let proof = NIMFSProof::<G1, G2, C1, C2, RO>::prove(
                &params.pp,
                &params.pp_secondary,
                &params.ro_config,
                &params.digest,
                (&params.shape, &params.shape_secondary),
                (&U, &W),
                (&U_secondary, &W_secondary),
                (&u, &w),
            )?;

            let input = NovaAugmentedCircuitInput::NonBase(NovaAugmentedCircuitNonBaseInput {
                vk: params.digest,
                i: G1::ScalarField::from(i.get()),
                z_0: z_0.clone(),
                z_i,
                U: U.clone(),
                U_secondary: U_secondary.clone(),
                u,
                proof: proof.0,
            });

            let (U, W) = proof.1;
            let (U_secondary, W_secondary) = proof.2;
            let i_next = i.saturating_add(1);

            (i_next, input, U, W, U_secondary, W_secondary)
        } else {
            let U = RelaxedR1CSInstance::<G1, C1>::new(&params.shape);
            let W = RelaxedR1CSWitness::zero(&params.shape);

            let U_secondary = RelaxedR1CSInstance::<G2, C2>::new(&params.shape_secondary);
            let W_secondary = RelaxedR1CSWitness::zero(&params.shape_secondary);

            let input = NovaAugmentedCircuitInput::<G1, G2, C1, C2, RO>::Base {
                vk: params.digest,
                z_0: z_0.clone(),
            };
            let i_next = NonZeroU64::new(1).unwrap();

            (i_next, input, U, W, U_secondary, W_secondary)
        };

        let cs = ConstraintSystem::new_ref();
        cs.set_mode(SynthesisMode::Prove {
            construct_matrices: false,
        });

        let circuit = NovaAugmentedCircuit::new(&params.ro_config, step_circuit, input);

        let z_i = tracing::debug_span!(target: LOG_TARGET, "satisfying_assignment")
            .in_scope(|| NovaConstraintSynthesizer::generate_constraints(circuit, cs.clone()))?;

        let cs_borrow = cs.borrow().unwrap();
        let witness = cs_borrow.witness_assignment.clone();
        let pub_io = cs_borrow.instance_assignment.clone();

        let w = R1CSWitness::<G1> { W: witness };

        let commitment_W = w.commit::<C1>(&params.pp);
        let u = R1CSInstance::<G1, C1> {
            commitment_W,
            X: pub_io,
        };

        let z_i = z_i.iter().map(R1CSVar::value).collect::<Result<_, _>>()?;

        Ok(Self {
            params,
            z_0,

            non_base: Some(IVCProofNonBase {
                U,
                W,
                U_secondary,
                W_secondary,
                u,
                w,
                i: i_next,
                z_i,
            }),
        })
    }

    pub fn verify(&self, num_steps: usize) -> Result<(), multifold::Error> {
        let _span = tracing::debug_span!(target: LOG_TARGET, "verify", %num_steps).entered();

        const NOT_SATISFIED_ERROR: multifold::Error =
            multifold::Error::R1CS(crate::r1cs::Error::NotSatisfied);

        let Some(non_base) = &self.non_base else {
            return Err(NOT_SATISFIED_ERROR);
        };

        let IVCProofNonBase {
            U,
            W,
            U_secondary,
            W_secondary,
            u,
            w,
            i,
            z_i,
        } = non_base;

        let num_steps = num_steps as u64;
        if num_steps != i.get() {
            return Err(NOT_SATISFIED_ERROR);
        }

        let mut random_oracle = RO::new(&self.params.ro_config);

        random_oracle.absorb(&self.params.digest);
        random_oracle.absorb(&G1::ScalarField::from(i.get()));
        random_oracle.absorb(&self.z_0);
        random_oracle.absorb(&z_i);
        random_oracle.absorb(U);
        random_oracle.absorb_non_native(U_secondary);

        let hash: &G1::ScalarField =
            &random_oracle.squeeze_field_elements(augmented::SQUEEZE_NATIVE_ELEMENTS_NUM)[0];

        if hash != &u.X[1] {
            return Err(NOT_SATISFIED_ERROR);
        }

        self.params
            .shape
            .is_relaxed_satisfied(U, W, &self.params.pp)?;
        self.params.shape_secondary.is_relaxed_satisfied(
            U_secondary,
            W_secondary,
            &self.params.pp_secondary,
        )?;
        self.params.shape.is_satisfied(u, w, &self.params.pp)?;

        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{pedersen::PedersenCommitment, poseidon_config, LOG_TARGET as SUPERNOVA_TARGET};

    use ark_crypto_primitives::sponge::poseidon::PoseidonSponge;
    use ark_ff::Field;
    use ark_r1cs_std::fields::{fp::FpVar, FieldVar};
    use ark_relations::r1cs::{ConstraintSystemRef, SynthesisError};

    use tracing_subscriber::{
        filter, fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt,
    };

    #[derive(Debug, Default)]
    pub struct CubicCircuit<F: Field>(PhantomData<F>);

    impl<F: PrimeField> StepCircuit<F> for CubicCircuit<F> {
        const ARITY: usize = 1;

        fn generate_constraints(
            &self,
            _: ConstraintSystemRef<F>,
            _: &FpVar<F>,
            z: &[FpVar<F>],
        ) -> Result<Vec<FpVar<F>>, SynthesisError> {
            assert_eq!(z.len(), 1);

            let x = &z[0];

            let x_square = x.square()?;
            let x_cube = x_square * x;

            let y: FpVar<F> = x + x_cube + &FpVar::Constant(5u64.into());

            Ok(vec![y])
        }
    }

    #[test]
    fn ivc_base_step() {
        ivc_base_step_with_cycle::<
            ark_pallas::PallasConfig,
            ark_vesta::VestaConfig,
            PedersenCommitment<ark_pallas::Projective>,
            PedersenCommitment<ark_vesta::Projective>,
        >()
        .unwrap()
    }

    fn ivc_base_step_with_cycle<G1, G2, C1, C2>() -> Result<(), multifold::Error>
    where
        G1: SWCurveConfig,
        G2: SWCurveConfig<BaseField = G1::ScalarField, ScalarField = G1::BaseField>,
        G1::BaseField: PrimeField + Absorb,
        G2::BaseField: PrimeField + Absorb,
        C1: CommitmentScheme<Projective<G1>, Commitment = Projective<G1>, SetupAux = ()>,
        C2: CommitmentScheme<Projective<G2>, Commitment = Projective<G2>, SetupAux = ()>,
    {
        let ro_config = poseidon_config();

        let circuit = CubicCircuit::<G1::ScalarField>(PhantomData);
        let z_0 = vec![G1::ScalarField::ONE];
        let num_steps = 1;

        let params = PublicParams::<
            G1,
            G2,
            C1,
            C2,
            PoseidonSponge<G1::ScalarField>,
            CubicCircuit<G1::ScalarField>,
        >::setup(ro_config, &circuit, &(), &())?;

        let mut recursive_snark = IVCProof::new(&params, &z_0);
        recursive_snark = recursive_snark.prove_step(&circuit)?;
        recursive_snark.verify(num_steps).unwrap();

        assert_eq!(&recursive_snark.z_i()[0], &G1::ScalarField::from(7));

        Ok(())
    }

    #[test]
    fn ivc_multiple_steps() {
        ivc_multiple_steps_with_cycle::<
            ark_pallas::PallasConfig,
            ark_vesta::VestaConfig,
            PedersenCommitment<ark_pallas::Projective>,
            PedersenCommitment<ark_vesta::Projective>,
        >()
        .unwrap()
    }

    fn ivc_multiple_steps_with_cycle<G1, G2, C1, C2>() -> Result<(), multifold::Error>
    where
        G1: SWCurveConfig,
        G2: SWCurveConfig<BaseField = G1::ScalarField, ScalarField = G1::BaseField>,
        G1::BaseField: PrimeField + Absorb,
        G2::BaseField: PrimeField + Absorb,
        C1: CommitmentScheme<Projective<G1>, Commitment = Projective<G1>, SetupAux = ()>,
        C2: CommitmentScheme<Projective<G2>, Commitment = Projective<G2>, SetupAux = ()>,
    {
        let filter = filter::Targets::new().with_target(SUPERNOVA_TARGET, tracing::Level::DEBUG);
        let _guard = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer().with_span_events(FmtSpan::ENTER | FmtSpan::CLOSE),
            )
            .with(filter)
            .set_default();

        let ro_config = poseidon_config();

        let circuit = CubicCircuit::<G1::ScalarField>(PhantomData);
        let z_0 = vec![G1::ScalarField::ONE];
        let num_steps = 3;

        let params = PublicParams::<
            G1,
            G2,
            C1,
            C2,
            PoseidonSponge<G1::ScalarField>,
            CubicCircuit<G1::ScalarField>,
        >::setup(ro_config, &circuit, &(), &())?;

        let mut recursive_snark = IVCProof::new(&params, &z_0);

        for _ in 0..num_steps {
            recursive_snark = IVCProof::prove_step(recursive_snark, &circuit)?;
        }
        recursive_snark.verify(num_steps).unwrap();

        assert_eq!(&recursive_snark.z_i()[0], &G1::ScalarField::from(44739235));
        Ok(())
    }
}