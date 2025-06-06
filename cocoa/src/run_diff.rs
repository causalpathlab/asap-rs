use crate::collapse_data::*;
use crate::common::*;
use crate::util::*;

use asap_alg::collapse_data::CollapsingOps;
use asap_alg::random_projection::RandProjOps;

pub use clap::Parser;

use matrix_param::traits::ParamIo;
use matrix_util::common_io;
pub use matrix_util::common_io::{extension, read_lines, read_lines_of_words};
use matrix_util::dmatrix_util::concatenate_vertical;
use matrix_util::traits::IoOps;
pub use std::sync::Arc;

use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Parser, Debug, Clone)]
pub struct DiffArgs {
    /// data files of either `.zarr` or `.h5` format. All the formats
    /// in the given list should be identical. We can convert `.mtx`
    /// to `.zarr` or `.h5` using `asap-data build` command.
    #[arg(required = true)]
    data_files: Vec<Box<str>>,

    /// sample membership files (comma-separated file names). Each sample
    /// file should match with each data file.
    #[arg(long, short, value_delimiter(','))]
    sample_files: Vec<Box<str>>,

    /// latent topic assignment files (comma-separated file names)
    /// Each topic file should match with each data file or None.
    #[arg(long, short, value_delimiter(','))]
    topic_assignment_files: Option<Vec<Box<str>>>,

    /// each line corresponds to (1) sample name and (2) exposure name
    #[arg(long, short)]
    exposure_assignment_file: Box<str>,

    /// #k-nearest neighbours within each condition
    #[arg(long, short = 'n', default_value_t = 10)]
    knn: usize,

    /// projection dimension to account for confounding factors.
    #[arg(long, short = 'p', default_value_t = 10)]
    proj_dim: usize,

    /// block_size for parallel processing
    #[arg(long, default_value_t = 100)]
    block_size: usize,

    /// number of iterations for optimization
    #[arg(long)]
    num_opt_iter: Option<usize>,

    /// hyperparameter a0 in Gamma(a0,b0)
    #[arg(long, default_value_t = 1.0)]
    a0: f32,

    /// hyperparameter b0 in Gamma(a0,b0)
    #[arg(long, default_value_t = 1.0)]
    b0: f32,

    /// output directory
    #[arg(long, short, required = true)]
    out: Box<str>,

    /// verbosity
    #[arg(long, short)]
    verbose: bool,
}

/// Run CoCoA differential analysis
pub fn run_cocoa_diff(args: DiffArgs) -> anyhow::Result<()> {
    if args.verbose {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    let mut data = read_input_data(args.clone())?;

    if data.cell_topic.ncols() > 1 {
        info!("normalizing cell topic proportion");
        data.cell_topic
            .row_iter_mut()
            .for_each(|mut r| r.unscale_mut(r.sum()));
    }

    info!("exposure names:");
    let exposure_names = data.exposure_names;
    for x in exposure_names.iter() {
        info!("{}", x);
    }

    let gene_names = data.sparse_data.row_names()?;

    let sample_to_cells = (0..data.sparse_data.num_batches())
        .map(|b| data.sparse_data.batch_to_columns(b).unwrap().clone())
        .collect::<Vec<_>>();

    let cocoa_input = &CocoaCollapseIn {
        n_genes: data.sparse_data.num_rows()?,
        n_samples: data.sparse_data.num_batches(),
        n_topics: data.cell_topic.ncols(),
        knn: args.knn,
        n_opt_iter: args.num_opt_iter,
        hyper_param: Some((args.a0, args.b0)),
        cell_topic_nk: data.cell_topic,
        sample_to_cells: &sample_to_cells,
        sample_to_exposure: &data.sample_to_exposure,
    };

    info!("Collecting statistics...");
    let cocoa_stat = data.sparse_data.collect_stat(cocoa_input)?;

    info!("Optimizing parameters...");
    let parameters = cocoa_stat.estimate_parameters()?;

    info!("Writing down the estimates...");
    for (k, param) in parameters.iter().enumerate() {
        let tau = &param.exposure;
        let outfile = format!("{}/tau_{}.summary.tsv.gz", args.out, k);
        common_io::mkdir(&outfile)?;
        tau.to_summary_stat_tsv(gene_names.clone(), exposure_names.clone(), &outfile)?;

        let tsv_header = format!("{}/mu_{}", args.out, k);
        param.shared.to_tsv(&tsv_header)?;

        let tsv_header = format!("{}/delta_{}", args.out, k);
        param.residual.to_tsv(&tsv_header)?;
    }

    info!("Done");
    Ok(())
}

struct DiffData {
    sparse_data: SparseIoVec,
    sample_to_exposure: Vec<usize>,
    exposure_names: Vec<Box<str>>,
    cell_topic: Mat,
}

fn read_input_data(args: DiffArgs) -> anyhow::Result<DiffData> {
    // push data files and collect batch membership
    let file = args.data_files[0].as_ref();
    let backend = match extension(file)?.to_string().as_str() {
        "h5" => SparseIoBackend::HDF5,
        "zarr" => SparseIoBackend::Zarr,
        _ => SparseIoBackend::Zarr,
    };

    if args.sample_files.len() != args.data_files.len() {
        return Err(anyhow::anyhow!("# sample files != # of data files"));
    }

    let (exposure, _) = read_lines_of_words(&args.exposure_assignment_file, -1)?;

    let sample_to_exposure: HashMap<_, _> = exposure
        .into_iter()
        .filter(|w| w.len() > 1)
        .map(|w| (w[0].clone(), w[1].clone()))
        .collect();

    let exposure_id: HashMap<_, usize> = sample_to_exposure
        .values()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .enumerate()
        .map(|(id, val)| (val, id))
        .collect();

    let n_exposure = exposure_id.len();
    let mut exposure_names = vec![String::from("").into_boxed_str(); n_exposure];
    for (x, &id) in exposure_id.iter() {
        if id < n_exposure {
            exposure_names[id] = x.clone();
        }
    }
    info!("{} exposure groups", n_exposure);

    let mut sparse_data = SparseIoVec::new();
    let mut cell_to_sample = vec![];
    let mut topic_vec = vec![];

    let data_files = args.data_files;
    let sample_files = args.sample_files;
    let topic_files = match args.topic_assignment_files {
        Some(vec) => vec.into_iter().map(Some).collect(),
        None => vec![None; data_files.len()],
    };

    for ((this_data_file, sample_file), topic_file) in
        data_files.into_iter().zip(sample_files).zip(topic_files)
    {
        info!("Importing: {}, {}", this_data_file, sample_file);

        match extension(&this_data_file)?.as_ref() {
            "zarr" => {
                assert_eq!(backend, SparseIoBackend::Zarr);
            }
            "h5" => {
                assert_eq!(backend, SparseIoBackend::HDF5);
            }
            _ => return Err(anyhow::anyhow!("Unknown file format: {}", this_data_file)),
        };

        let this_data = open_sparse_matrix(&this_data_file, &backend)?;
        let ndata = this_data.num_columns().unwrap_or(0);

        let this_sample = read_lines(&sample_file)?;
        let this_topic = match topic_file.as_ref() {
            Some(_file) => Mat::from_tsv(_file, None)?,
            None => Mat::from_element(ndata, 1, 1.0),
        };

        if this_sample.len() != ndata || this_topic.nrows() != ndata {
            return Err(anyhow::anyhow!(
                "{} and {} don't match",
                sample_file,
                this_data_file,
            ));
        }

        cell_to_sample.extend(this_sample);
        sparse_data.push(Arc::from(this_data))?;
        topic_vec.push(this_topic);
    }
    info!("Total {} data sets combined", sparse_data.len());

    let cell_topic = concatenate_vertical(topic_vec.as_slice())?;

    let proj_out = sparse_data.project_columns(args.proj_dim, Some(args.block_size))?;
    let proj_kn = &proj_out.proj;
    sparse_data.register_batches(proj_kn, &cell_to_sample)?;

    let samples = sparse_data
        .batch_names()
        .ok_or(anyhow::anyhow!("empty sample name in sparse data"))?;

    let sample_to_exposure: Vec<usize> = samples
        .iter()
        .map(|s| {
            if let Some(exposure) = sample_to_exposure.get(s) {
                exposure_id[exposure]
            } else {
                warn!("No exposure was assigned for sample {}, but it's kept for controlling confounders.", s);
                n_exposure
            }
        })
        .collect();

    info!(
        "Found {} samples with exposure assignments",
        sample_to_exposure.len()
    );

    Ok(DiffData {
        sparse_data,
        sample_to_exposure,
        exposure_names,
        cell_topic,
    })
}
