use crate::sparse_io::*;
use log::info;
use matrix_util::common_io::*;
use std::ops::Range;
use std::sync::Arc;
use zarrs::array::DataType;
use zarrs::array_subset::ArraySubset;
use zarrs::filesystem::FilesystemStore;
use zarrs::storage::ReadableWritableListableStorageTraits as ZStorageTraits;

const NUM_CHUNKS: usize = 1000;
const MIN_CHUNK_SIZE: usize = 1000;
const COMPRESSION_LEVEL: i32 = 3;

/// 10x-like cell-feature matrix with `zarr` backend (feature x cell)
///
/// ```text
/// (root)
///     ├── nrow
///     ├── ncell
///     ├── by_column
///     │   ├── data
///     │   ├── indices (row indices)
///     │   └── indptr (column pointers)
///     └── by_row
///         ├── data
///         ├── indices (column indices)
///         └── indptr (row pointers)
/// ```
///
#[derive(Clone)]
pub struct SparseMtxData {
    pub store: Arc<dyn ZStorageTraits>,
    file_name: String,
    max_row_name_idx: usize,
    max_column_name_idx: usize,
    by_column_indptr: Vec<u64>,
    by_row_indptr: Vec<u64>,
}

#[allow(dead_code)]
impl SparseMtxData {
    /// Create an empty new `SparseMtxData` instance with a zarr
    /// backend file If no `backend_file` is provided, a temporary
    /// file will be created.
    ///
    /// * `backend_file` - Optional zarr backend file
    pub fn new(zarr_file: Option<&str>) -> anyhow::Result<Self> {
        let ret = match zarr_file {
            Some(backend_file) => Self::register_backend_file(backend_file)?,
            None => {
                let backend_file = create_temp_dir_file(".zarr")?;
                let backend_file = backend_file.to_str().expect("to_str failed");
                Self::register_backend_file(&backend_file)?
            }
        };
        Ok(ret)
    }

    /// Create `SparseMtxData` instance from an existing zarr backend file
    /// * `zarr_file` - zarr backend file
    pub fn open(backend_file: &str) -> anyhow::Result<Self> {
        let store = Arc::new(FilesystemStore::new(backend_file)?);

        if let (Some(nrow), Some(ncol), Some(nnz)) = (
            Self::_num_rows(store.clone()),
            Self::_num_columns(store.clone()),
            Self::_num_nnz(store.clone()),
        ) {
            info!("#rows: {}, #columns: {}, #non-zeros: {}", nrow, ncol, nnz);
        } else {
            anyhow::bail!("Couldn't figure out the size of this sparse matrix data");
        }

        let mut ret = Self {
            store: store.clone(),
            file_name: backend_file.to_string(),
            max_row_name_idx: MAX_ROW_NAME_IDX,
            max_column_name_idx: MAX_COLUMN_NAME_IDX,
            by_column_indptr: vec![],
            by_row_indptr: vec![],
        };

        ret.read_column_indptr()?;
        ret.read_row_indptr()?;

        Ok(ret)
    }

    /// Create `SparseMtxData` from mtx file with `backend_file` as
    /// the backend file.  If no `backend_file` is provided, it will
    /// be the same as `mtx_file` with `.h5` extension.
    /// * `mtx_file`: mtx file to be read into HDF5 backend
    /// * `backend_file`: HDF5 file to be associated with
    /// * `index_by_row`: if true, the matrix will be indexed by row
    pub fn from_mtx_file(
        mtx_file: &str,
        backend_file: Option<&str>,
        index_by_row: Option<bool>,
    ) -> anyhow::Result<Self> {
        let mut ret = match backend_file {
            Some(backend_file) => {
                info!("backend file : {}", backend_file);
                Self::register_backend_file(backend_file)?
            }
            None => {
                let backend_file = mtx_file.to_string() + ".zarr";
                info!("backend file : {}", backend_file);
                Self::register_backend_file(&backend_file)?
            }
        };

        // populate data from mtx file
        info!("importing mtx file by column");
        ret.import_mtx_file_by_col(mtx_file)?;
        ret.read_column_indptr()?;

        if Some(true) == index_by_row {
            info!("importing mtx file by row");
            ret.import_mtx_file_by_row(mtx_file)?;
            ret.read_row_indptr()?;
        }

        info!("created sparse backend from {}", mtx_file);
        Ok(ret)
    }

    /// Create a new `SparseMtxData` instance from an `ndarray` array
    /// * `array` - 2D array to be added to the backend
    /// * `backend_file` - Optional zarr backend file
    /// * `index_by_row` - Optional flag to index by row (CSR format)
    pub fn from_ndarray(
        array: &Array2<f32>,
        zarr_file: Option<&str>,
        index_by_row: Option<bool>,
    ) -> anyhow::Result<Self> {
        let mut ret = match zarr_file {
            Some(backend_file) => Self::register_backend_file(backend_file)?,
            None => {
                let backend_file = create_temp_dir_file(".zarr")?;
                let backend_file = backend_file.to_str().expect("to_str failed");
                Self::register_backend_file(&backend_file)?
            }
        };

        ret.import_ndarray_by_col(&array)?; // for column-wise
        ret.read_column_indptr()?; // pointers

        if Some(true) == index_by_row {
            ret.import_ndarray_by_row(&array)?;
            ret.read_row_indptr()?;
        }
        Ok(ret)
    }

    /// Create a new `SparseMtxData` instance from an `DMatrix` array
    /// * `array` - 2D array to be added to the backend
    /// * `backend_file` - Optional zarr backend file
    /// * `index_by_row` - Optional flag to index by row (CSR format)
    pub fn from_dmatrix(
        matrix: &DMatrix<f32>,
        zarr_file: Option<&str>,
        index_by_row: Option<bool>,
    ) -> anyhow::Result<Self> {
        let mut ret = match zarr_file {
            Some(backend_file) => Self::register_backend_file(backend_file)?,
            None => {
                let backend_file = create_temp_dir_file(".zarr")?;
                let backend_file = backend_file.to_str().expect("to_str failed");
                Self::register_backend_file(&backend_file)?
            }
        };

        ret.import_dmatrix_by_col(&matrix)?; // for column-wise
        ret.read_column_indptr()?; // pointers

        if Some(true) == index_by_row {
            ret.import_dmatrix_by_row(&matrix)?;
            ret.read_row_indptr()?;
        }
        Ok(ret)
    }

    /// Show the hierarchy of the zarr store
    pub fn print_hierarchy(self: &Self) -> anyhow::Result<()> {
        let node = zarrs::node::Node::open(&self.store, "/")?;
        let tree = node.hierarchy_tree();
        info!("hierarchy_tree:\n{}", tree);
        Ok(())
    }

    /// Helper function to create a new zarr backend file
    fn register_backend_file(zarr_file: &str) -> anyhow::Result<Self> {
        // dbg!(zarr_file);
        use zarrs::group::GroupBuilder;
        let store = Arc::new(FilesystemStore::new(zarr_file)?);
        let root = GroupBuilder::new().build(store.clone(), "/")?;
        root.store_metadata()?;

        Ok(Self {
            store: store.clone(),
            file_name: zarr_file.to_string(),
            max_row_name_idx: MAX_ROW_NAME_IDX,
            max_column_name_idx: MAX_COLUMN_NAME_IDX,
            by_column_indptr: vec![],
            by_row_indptr: vec![],
        })
    }

    //////////////////////
    // backend related  //
    //////////////////////

    /// Helper function to create a filled array(s) with the given
    /// data type and fill value. This is the most useful function to
    /// create a vector like data.
    ///
    /// * `key` - the key name
    /// * `dt` - the data type among `DataType`
    /// * `data` - `ndarray` to be stored
    ///
    fn new_filled_ndarray<V, S, D>(
        self: &mut Self,
        key: &str,
        dt: DataType,
        data: ndarray::ArrayBase<S, D>,
    ) -> anyhow::Result<()>
    where
        V: zarrs::array::Element + Default + Clone,
        S: ndarray::Data<Elem = V> + Clone,
        D: ndarray::Dimension + ndarray::RemoveAxis,
    {
        use zarrs::array::codec::ZstdCodec;
        use zarrs::array::ArrayBuilder;
        use zarrs::array::DataType;
        use zarrs::array::FillValue;

        let fill = match dt {
            DataType::Float32 => FillValue::from(zarrs::array::ZARR_NAN_F32),
            DataType::UInt64 => FillValue::from(0u64),
            DataType::String => FillValue::from(""),
            _ => FillValue::from(0),
        };

        let nchunks = NUM_CHUNKS;
        let array_shape: Vec<u64> = data.shape().iter().map(|&x| x as u64).collect();
        let chunk_size: Vec<u64> = data
            .shape()
            .iter()
            .map(|&d| (d / nchunks).max(MIN_CHUNK_SIZE).min(d) as u64)
            .collect();

        let array = ArrayBuilder::new(
            array_shape,            // array shape
            dt.into(),              // data type
            chunk_size.try_into()?, // chunk shape
            fill,                   //
        )
        .bytes_to_bytes_codecs(vec![Arc::new(ZstdCodec::new(COMPRESSION_LEVEL, false))])
        .build(self.store.clone(), key)?;

        array.store_array_subset_ndarray(array.subset_all().start(), data.to_owned())?;
        array.store_metadata()?;
        Ok(())
    }

    /// Helper function to create a filled 1D array with the given
    /// data type and fill value. This is the most useful function to
    /// create a vector like data.
    ///
    /// * `key` - the key name
    /// * `dt` - the data type among `DataType`
    /// * `vec` - the vector to be stored
    ///
    fn new_filled_vector<V>(
        self: &mut Self,
        key: &str,
        dt: DataType,
        vec: Vec<V>,
    ) -> anyhow::Result<()>
    where
        V: zarrs::array::Element,
    {
        use zarrs::array::codec::ZstdCodec;
        use zarrs::array::ArrayBuilder;
        use zarrs::array::DataType;
        use zarrs::array::FillValue;
        // use zarrs::array::ZARR_NAN_F32;

        let nelem = vec.len();
        let nchunks = NUM_CHUNKS;
        let chunk_size = (nelem / nchunks).max(MIN_CHUNK_SIZE).min(nelem);

        let fill = match dt {
            DataType::Float32 => FillValue::from(zarrs::array::ZARR_NAN_F32),
            DataType::UInt64 => FillValue::from(0u64),
            DataType::String => FillValue::from(""),
            _ => FillValue::from(0),
        };

        let array = ArrayBuilder::new(
            vec![vec.len() as u64],              // array shape
            dt.into(),                           // data type
            vec![chunk_size as u64].try_into()?, // chunk shape
            fill,                                //
        )
        .bytes_to_bytes_codecs(vec![Arc::new(ZstdCodec::new(COMPRESSION_LEVEL, false))])
        .build(self.store.clone(), key)?;

        array.store_metadata()?;

        let ntot = vec.len() as u64;
        let subset = ArraySubset::new_with_ranges(&[0..ntot]);
        array.store_array_subset_elements(&subset, &vec)?;

        Ok(())
    }

    fn _open_vector(
        self: &Self,
        key: &str,
    ) -> anyhow::Result<zarrs::array::Array<dyn ZStorageTraits>> {
        use zarrs::array::Array as ZArray;
        let ret = ZArray::open(self.store.clone(), key)?;
        Ok(ret)
    }

    fn open_csc_triplets(
        &self,
    ) -> anyhow::Result<(
        zarrs::array::Array<dyn ZStorageTraits>,
        zarrs::array::Array<dyn ZStorageTraits>,
        zarrs::array::Array<dyn ZStorageTraits>,
    )> {
        Ok((
            self._open_vector("/by_column/indptr")?,
            self._open_vector("/by_column/data")?,
            self._open_vector("/by_column/indices")?,
        ))
    }

    fn open_csr_triplets(
        &self,
    ) -> anyhow::Result<(
        zarrs::array::Array<dyn ZStorageTraits>,
        zarrs::array::Array<dyn ZStorageTraits>,
        zarrs::array::Array<dyn ZStorageTraits>,
    )> {
        Ok((
            self._open_vector("/by_row/indptr")?,
            self._open_vector("/by_row/data")?,
            self._open_vector("/by_row/indices")?,
        ))
    }

    fn _retrieve_vector<V>(self: &Self, key: &str) -> anyhow::Result<Vec<V>>
    where
        V: zarrs::array::ElementOwned,
    {
        let data = self._open_vector(key)?;
        let ntot = data.shape()[0];
        let subset = ArraySubset::new_with_ranges(&[0..ntot]);
        Ok(data.retrieve_array_subset_elements::<V>(&subset)?)
    }

    /////////////////////////////
    // purely helper functions //
    /////////////////////////////

    /// Helper function to set an attribute from a group named `group_name`
    fn _set_group_attr<V>(
        store: Arc<dyn ZStorageTraits>,
        group_name: &str,
        attr_name: &str,
        value: &V,
    ) -> anyhow::Result<()>
    where
        V: serde::Serialize,
    {
        use zarrs::group::Group;
        let mut group = Group::open(store, group_name)?;

        let new_value = serde_json::to_value(value)?;
        group
            .attributes_mut()
            .insert((*attr_name).to_string(), new_value);
        group.store_metadata()?;
        Ok(())
    }

    /// Helper function to get an attribute from a group named `group_name`
    fn _get_group_attr<V>(
        store: Arc<dyn ZStorageTraits>,
        group_name: &str,
        attr_name: &str,
    ) -> Option<V>
    where
        V: serde::de::DeserializeOwned,
    {
        zarrs::group::Group::open(store, group_name)
            .ok()
            .and_then(|grp| grp.attributes().get(attr_name).cloned())
            .and_then(|attr| serde_json::from_value(attr.clone()).ok())
    }

    fn _num_nnz(store: Arc<dyn ZStorageTraits>) -> Option<usize> {
        Self::_get_group_attr::<usize>(store.clone(), "/", "nnz")
    }

    fn _num_rows(store: Arc<dyn ZStorageTraits>) -> Option<usize> {
        Self::_get_group_attr::<usize>(store.clone(), "/", "nrow")
    }

    fn _num_columns(store: Arc<dyn ZStorageTraits>) -> Option<usize> {
        Self::_get_group_attr::<usize>(store.clone(), "/", "ncol")
    }
    /// Helper function to add a group in `self.store`
    fn _add_group(self: &mut Self, group_name: &str) -> anyhow::Result<()> {
        use zarrs::group::Group;

        if Group::open(self.store.clone(), group_name).is_err() {
            let new_group =
                zarrs::group::GroupBuilder::new().build(self.store.clone(), group_name)?;
            new_group.store_metadata()?;
        } else {
            dbg!("group already exists");
        }

        Ok(())
    }
}

impl SparseIo for SparseMtxData {
    type IndexIter = Vec<usize>;

    /// Read row index pointers
    fn read_row_indptr(self: &mut Self) -> anyhow::Result<()> {
        use zarrs::array::Array as Zarray;
        let key = "/by_row/indptr";
        if let Ok(indptr) = Zarray::open(self.store.clone(), key) {
            let indptr_vec = indptr.retrieve_array_subset_elements::<u64>(&indptr.subset_all())?;
            self.by_row_indptr.clear();
            self.by_row_indptr.extend(indptr_vec);
        }
        Ok(())
    }

    /// Read column index pointers
    fn read_column_indptr(self: &mut Self) -> anyhow::Result<()> {
        use zarrs::array::Array as ZArray;
        let key = "/by_column/indptr";
        if let Ok(indptr) = ZArray::open(self.store.clone(), key) {
            let indptr_vec = indptr.retrieve_array_subset_elements::<u64>(&indptr.subset_all())?;
            self.by_column_indptr.clear();
            self.by_column_indptr.extend(indptr_vec);
        }
        Ok(())
    }

    /// Helper function to keep the matrix shape
    fn record_mtx_shape(
        self: &mut Self,
        mtx_shape: Option<(usize, usize, usize)>,
    ) -> anyhow::Result<()> {
        let check_set_attr = |attr_name: &str, value: usize| -> anyhow::Result<()> {
            let old_value = Self::_get_group_attr::<usize>(self.store.clone(), "/", attr_name);
            let new_value = serde_json::to_value(value)?;

            match old_value {
                Some(old_value) => {
                    if old_value != new_value {
                        return Err(anyhow::anyhow!("{} mismatch", attr_name));
                    }
                }
                _ => {
                    Self::_set_group_attr(self.store.clone(), "/", attr_name, &new_value)?;
                }
            }
            Ok(())
        };

        if let Some((nrow, ncol, nnz)) = mtx_shape {
            check_set_attr("nrow", nrow)?;
            check_set_attr("ncol", ncol)?;
            check_set_attr("nnz", nnz)?;
        }
        Ok(())
    }

    /// Helper function to create a new zarr backend file
    fn initialize_backend(&mut self) -> anyhow::Result<()> {
        use zarrs::group::GroupBuilder;

        self.remove_backend_file()?;
        let zarr_file = &self.file_name;
        let store = Arc::new(FilesystemStore::new(zarr_file)?);
        let root = GroupBuilder::new().build(store.clone(), "/")?;
        root.store_metadata()?;

        self.store = store.clone();
        self.file_name = zarr_file.to_string().clone();
        self.max_column_name_idx = MAX_COLUMN_NAME_IDX;
        self.max_row_name_idx = MAX_ROW_NAME_IDX;
        self.by_column_indptr = vec![];
        self.by_row_indptr = vec![];

        Ok(())
    }

    /// Clean up the backend file
    fn remove_backend_file(&self) -> anyhow::Result<()> {
        let backend = std::path::Path::new(&self.file_name);
        if backend.exists() {
            std::fs::remove_dir_all(backend)?;
        }
        Ok(())
    }

    /// Access file name of the zarr backend
    fn get_backend_file_name(self: &Self) -> &str {
        &self.file_name
    }

    /// Export the data to a mtx file. This will take time.
    /// * `mtx_file`: mtx file to be written
    fn to_mtx_file(&self, mtx_file: &str) -> anyhow::Result<()> {
        if let (Some(ncol), Some(nrow), Some(nnz)) =
            (self.num_columns(), self.num_rows(), self.num_non_zeros())
        {
            let (nrow, ncol, nnz) = (nrow as usize, ncol as usize, nnz as usize);

            let mut buf = open_buf_writer(mtx_file)?;
            writeln!(buf, "%%MatrixMarket matrix coordinate real general")?;
            writeln!(buf, "{}\t{}\t{}", nrow, ncol, nnz)?;

            let (indptr, data, indices) = self.open_csc_triplets()?;
            let indptr = indptr.retrieve_array_subset_ndarray::<u64>(&indptr.subset_all())?;
            debug_assert!(indptr.len() == ncol + 1);

            for jj in 0..ncol {
                let (start, end) = (indptr[jj], indptr[jj + 1]);
                let subset = ArraySubset::new_with_ranges(&[start..end]);
                let data_slice = data.retrieve_array_subset_ndarray::<f32>(&subset)?;
                let indices_slice = indices.retrieve_array_subset_ndarray::<u64>(&subset)?;

                // write them with 1-based indices
                for k in 0..(end - start) {
                    let val = data_slice[k as usize];
                    let ii = indices_slice[k as usize] as usize;
                    writeln!(buf, "{}\t{}\t{}", ii + 1, jj + 1, val)?;
                }
            }
            buf.flush()?;
            Ok(())
        } else {
            return Err(anyhow::anyhow!(
                "Unable to figure out the size of the backend data"
            ));
        }
    }

    /// Set row names for the matrix
    /// * `row_name_file`: a file each line contains row name words
    fn register_row_names_file(self: &mut Self, row_name_file: &str) {
        self.register_names_file(
            "/row_names",
            row_name_file,
            0..self.max_row_name_idx,
            ROW_SEP,
        )
        .expect("failed to add row names");
    }

    /// Set row names for the matrix
    /// * `rows`: a vector of row names
    fn register_row_names_vec(&mut self, rows: &Vec<Box<str>>) {
        self.register_names_vec("/row_names", rows)
            .expect("failed to add row names");
    }

    /// Set column names for the matrix
    /// * `column_name_file`: a file each line contains column name words
    fn register_column_names_file(self: &mut Self, column_name_file: &str) {
        self.register_names_file(
            "/column_names",
            column_name_file,
            0..self.max_column_name_idx,
            COLUMN_SEP,
        )
        .expect("failed to add column names");
    }

    /// Set column names for the matrix
    /// * `columns`: a vector of column names
    fn register_column_names_vec(&mut self, columns: &Vec<Box<str>>) {
        self.register_names_vec("/column_names", columns)
            .expect("failed to add column names");
    }

    /// Number of rows in the matrix
    fn num_rows(self: &Self) -> Option<usize> {
        Self::_num_rows(self.store.clone())
    }

    /// Number of columns in the matrix
    fn num_columns(self: &Self) -> Option<usize> {
        Self::_num_columns(self.store.clone())
    }

    /// Number of non-zero elements in the matrix
    fn num_non_zeros(self: &Self) -> Option<usize> {
        Self::_num_nnz(self.store.clone())
    }

    /// Add arbitrary names (a vector of strings)
    /// * `group_name`: group name
    /// * `name_file`: a file each line contains name words
    /// * `name_columns`: range of columns to be used for name
    /// * `name_sep`: separator for name columns
    fn register_names_file(
        self: &mut Self,
        key: &str,
        name_file: &str,
        name_columns: Range<usize>,
        name_sep: &str,
    ) -> anyhow::Result<()> {
        let (_names, _) = read_lines_of_words(name_file, -1)?;

        let name_columns = name_columns.clone().collect::<Vec<_>>();

        let _names: Vec<String> = _names
            .iter()
            .map(|x| {
                name_columns
                    .iter()
                    .filter_map(|&i| x.get(i))
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join(name_sep)
                    .parse()
                    .expect("invalid name")
            })
            .collect();

        self.new_filled_vector(key, DataType::String, _names)?;
        Ok(())
    }

    /// Add arbitrary names (a vector of strings)
    /// * `group_name`: group name
    /// * `names`: a file each line contains name words
    fn register_names_vec(&mut self, key: &str, names: &Vec<Box<str>>) -> anyhow::Result<()> {
        let _names = names.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        self.new_filled_vector(key, DataType::String, _names)?;
        Ok(())
    }

    fn row_names(&self) -> anyhow::Result<Vec<Box<str>>> {
        self.retrieve_registered_names("/row_names")
    }

    fn column_names(&self) -> anyhow::Result<Vec<Box<str>>> {
        self.retrieve_registered_names("/column_names")
    }

    /// Get back the registered names
    /// * `key`: key for the registered names
    fn retrieve_registered_names(&self, key: &str) -> anyhow::Result<Vec<Box<str>>> {
        Ok(self
            ._retrieve_vector::<String>(key)?
            .into_iter()
            .map(|s| s.into_boxed_str())
            .collect())
    }

    /// Read columns within the range and return a vector of triplets (row, col, value)
    /// * `col` : usize
    ///
    fn read_triplets_by_single_column(
        &self,
        j_data: usize,
    ) -> anyhow::Result<(usize, usize, Vec<(usize, usize, f32)>)> {
        use zarrs::array::Array as ZArray;
        use zarrs::array_subset::ArraySubset;

        debug_assert!(self.by_column_indptr.len() > 0);
        let indptr = &self.by_column_indptr;

        let key = "/by_column/data";
        let data = ZArray::open(self.store.clone(), key)?;
        let key = "/by_column/indices";
        let indices = ZArray::open(self.store.clone(), key)?;

        if let (Some(ncol), Some(nrow)) = (self.num_columns(), self.num_rows()) {
            let mut ret: Vec<(usize, usize, f32)> = Vec::new();
            let ncol_out = 1;
            let jj = 0;

            debug_assert!(indptr.len() > ncol);

            if j_data < ncol {
                debug_assert!((j_data + 1) < indptr.len());

                // [start, end)
                let start = indptr[j_data];
                let end = indptr[j_data + 1];

                if start < end {
                    let subset = ArraySubset::new_with_ranges(&[start..end]);

                    let data_slice = data.retrieve_array_subset_elements::<f32>(&subset)?;
                    let indices_slice = indices.retrieve_array_subset_elements::<u64>(&subset)?;

                    for k in 0..(end - start) {
                        let x_ij = data_slice[k as usize];
                        let ii = indices_slice[k as usize] as usize;
                        debug_assert!(ii < nrow);
                        ret.push((ii, jj, x_ij));
                    }
                }
            }

            Ok((nrow, ncol_out, ret))
        } else {
            return Err(anyhow::anyhow!(
                "Unable to figure out the size of the backend data"
            ));
        }
    }

    /// Read columns within the range and return dense `ndarray::Array2`
    /// * `columns` : range e.g., 0..3 -> [0, 1, 2] or vec![0, 1, 2]
    ///
    fn read_triplets_by_columns(
        self: &Self,
        columns: Self::IndexIter,
    ) -> anyhow::Result<(usize, usize, Vec<(usize, usize, f32)>)> {
        let mut ret: Vec<(usize, usize, f32)> = Vec::new();

        use zarrs::array::Array as ZArray;
        use zarrs::array_subset::ArraySubset;

        debug_assert!(self.by_column_indptr.len() > 0);
        let indptr = &self.by_column_indptr;
        let columns_vec = columns.into_iter().collect::<Vec<usize>>();

        let key = "/by_column/data";
        let data = ZArray::open(self.store.clone(), key)?;
        let key = "/by_column/indices";
        let indices = ZArray::open(self.store.clone(), key)?;

        if let (Some(ncol), Some(nrow)) = (self.num_columns(), self.num_rows()) {
            let ncol_out = columns_vec.len();

            debug_assert!(indptr.len() > ncol);

            for (jj, &j_data) in columns_vec.iter().enumerate() {
                if j_data < ncol {
                    debug_assert!((j_data + 1) < indptr.len());

                    // [start, end)
                    let start = indptr[j_data];
                    let end = indptr[j_data + 1];

                    if start < end {
                        let subset = ArraySubset::new_with_ranges(&[start..end]);

                        let data_slice = data.retrieve_array_subset_elements::<f32>(&subset)?;
                        let indices_slice =
                            indices.retrieve_array_subset_elements::<u64>(&subset)?;

                        for k in 0..(end - start) {
                            let x_ij = data_slice[k as usize];
                            let ii = indices_slice[k as usize] as usize;
                            debug_assert!(ii < nrow);
                            ret.push((ii, jj, x_ij));
                        }
                    }
                }
            }
            Ok((nrow, ncol_out, ret))
        } else {
            return Err(anyhow::anyhow!(
                "Unable to figure out the size of the backend data"
            ));
        }
    }

    /// Read rows within the range and return a vector of triplets (row, col, value)
    /// * `rows` : range e.g., 0..3 -> [0, 1, 2] or vec![0, 1, 2]
    ///
    fn read_triplets_by_rows(
        &self,
        rows: Self::IndexIter,
    ) -> anyhow::Result<(usize, usize, Vec<(usize, usize, f32)>)> {
        use zarrs::array::Array as ZArray;
        use zarrs::array_subset::ArraySubset;

        debug_assert!(self.by_row_indptr.len() > 0);
        let indptr = &self.by_row_indptr;

        let rows_vec = rows.into_iter().collect::<Vec<usize>>();

        let key = "/by_row/data";
        let data = ZArray::open(self.store.clone(), key)?;
        let key = "/by_row/indices";
        let indices = ZArray::open(self.store.clone(), key)?;

        if let (Some(nrow), Some(ncol)) = (self.num_rows(), self.num_columns()) {
            let nrow_out = rows_vec.len();
            let ncol = ncol as usize;

            debug_assert!(indptr.len() > nrow);

            let mut ret: Vec<(usize, usize, f32)> = Vec::new();

            for (ii, &i_data) in rows_vec.iter().enumerate() {
                if i_data < nrow {
                    debug_assert!((i_data + 1) < indptr.len());

                    // [start, end)
                    let start = indptr[i_data];
                    let end = indptr[i_data + 1];

                    if start < end {
                        let subset = ArraySubset::new_with_ranges(&[start..end]);
                        let data_slice = data.retrieve_array_subset_elements::<f32>(&subset)?;
                        let indices_slice =
                            indices.retrieve_array_subset_elements::<u64>(&subset)?;

                        for k in 0..(end - start) {
                            let x_ij = data_slice[k as usize];
                            let jj = indices_slice[k as usize] as usize;
                            debug_assert!(jj < ncol);
                            ret.push((ii, jj, x_ij));
                        }
                    }
                }
            }
            Ok((nrow_out, ncol, ret))
        } else {
            return Err(anyhow::anyhow!(
                "Unable to figure out the size of the backend data"
            ));
        }
    }
    /// CSR data structure in Zarr backend
    ///
    /// ```text
    ///     └── by_row
    ///         ├── data
    ///         ├── indices (column indices)
    ///         └── isndptr (row pointers)
    /// ```
    fn record_csr_dataset_backend(
        self: &mut Self,
        csr_cols: &Vec<u64>,
        csr_vals: &Vec<f32>,
        csr_rowptr: &Vec<u64>,
    ) -> anyhow::Result<()> {
        // open or create the group "/by_row"
        let key = "/by_row";
        self._add_group(key)?;

        let key = "/by_row/data";
        self.new_filled_vector(key, DataType::Float32, csr_vals.clone())?;
        let key = "/by_row/indices";
        self.new_filled_vector(key, DataType::UInt64, csr_cols.clone())?;
        let key = "/by_row/indptr";
        self.new_filled_vector(key, DataType::UInt64, csr_rowptr.clone())?;

        Ok(())
    }

    /// CSC data structure in Zarr backend
    ///
    /// ```text
    /// Helper function to record the CSC dataset
    ///     ├── by_column
    ///     │   ├── data
    ///     │   ├── indices (row indices)
    ///     │   └── indptr (column pointers)
    /// ```
    fn record_csc_dataset_backend(
        self: &mut Self,
        csc_rows: &Vec<u64>,
        csc_vals: &Vec<f32>,
        csc_colptr: &Vec<u64>,
    ) -> anyhow::Result<()> {
        // open or create the group "/by_column"
        let key = "/by_column";
        self._add_group(key)?;

        let key = "/by_column/data";
        self.new_filled_vector(key, DataType::Float32, csc_vals.clone())?;
        let key = "/by_column/indices";
        // dbg!(key);
        self.new_filled_vector(key, DataType::UInt64, csc_rows.clone())?;

        let key = "/by_column/indptr";
        // dbg!(key);
        self.new_filled_vector(key, DataType::UInt64, csc_colptr.clone())?;

        Ok(())
    }
}
