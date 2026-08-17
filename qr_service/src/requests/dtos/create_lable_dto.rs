use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
pub struct LabelInfo {
    pub kind: i32,
    pub customer_name: String,
    pub part_no: String,
    pub material_name: String,
    pub qr_string: String,
    pub is_return: bool,
}
