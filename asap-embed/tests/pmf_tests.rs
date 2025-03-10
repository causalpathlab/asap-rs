// use asap_embed::candle_etm::*;
use asap_embed::candle_aux_layers::*;
use asap_embed::candle_data_loader::*;
use asap_embed::candle_vae_inference::*;
use asap_embed::candle_loss_functions::*;
use asap_embed::candle_model_decoder::*;
use asap_embed::candle_model_encoder::*;
use candle_core::{DType, Device, Tensor};
use candle_nn::Module;
use candle_nn::{VarBuilder, VarMap};
use matrix_util::traits::SampleOps;

#[test]
fn aux_layer() -> anyhow::Result<()> {
    let mut stack = StackLayers::<candle_nn::Linear>::new();

    let vm = candle_nn::VarMap::new();
    let vs = candle_nn::VarBuilder::from_varmap(&vm, DType::F32, &candle_core::Device::Cpu);

    stack.push_with_act(
        candle_nn::linear(5, 10, vs.pp("lin1"))?,
        candle_nn::Activation::Relu,
    );

    stack.push_with_act(
        candle_nn::linear(10, 5, vs.pp("lin2"))?,
        candle_nn::Activation::Relu,
    );

    use rayon::prelude::*;

    (0..10).into_par_iter().for_each(|_| {
        let x = Tensor::rgamma(100, 5, (1.0, 1.0));
        let y = stack.forward(&x).unwrap();
        println!("{:?}", y);
    });

    Ok(())
}

#[test]
fn pmf() -> anyhow::Result<()> {
    std::env::set_var("RUST_LOG", "info");
    env_logger::init();

    let dev = Device::new_metal(0)?;

    let dd = 1000_usize;
    let nn = 500_usize;
    let kk = 5_usize;

    let beta_dk = Tensor::rgamma(dd, kk, (1.0, 1.0));
    let theta_nk = Tensor::rgamma(nn, kk, (1.0, 1.0));
    let y = beta_dk.matmul(&theta_nk.transpose(0, 1)?)?;

    //////////////////////////////////////////////////
    // it's a torch-like system, sample by features //
    //////////////////////////////////////////////////

    let x_nd = y.transpose(0, 1)?.to_device(&dev)?;

    ///////////////////
    // build a model //
    ///////////////////

    let vm = VarMap::new();
    let vs = VarBuilder::from_varmap(&vm, DType::F32, &dev);

    let kk = 10;
    let layers = vec![128, 128];
    let enc = NonNegEncoder::new(dd, kk, &layers, vs.clone())?;
    let dec = TopicDecoder::new(dd, kk, vs.clone())?;

    let mut vae = Vae::build(&enc, &dec, &vm);

    let mut data_loader = InMemoryData::from(&x_nd)?;

    let _llik = vae.train(
        &mut data_loader,
        &topic_likelihood,
        &TrainConfig {
            learning_rate: 5e-3,
            batch_size: 100,
            num_epochs: 1000,
            device: dev,
            verbose: true,
        },
    )?;

    let (z_nk, _kl) = vae.encoder.forward_t(&x_nd, false)?;

    // matrix_util::tensor_io::write_tsv(&z_nk, "z_nk.txt.gz")?;

    // let vm = vm.data().lock().expect("failed to lock varmap");

    // for (k, v) in vm.iter() {
    //     println!("{}: {:?}", k, v.shape());
    // }

    Ok(())
}
