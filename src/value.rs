use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Text(String),
    Vector(Vec<f32>),
    Point(Point),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub lon: f64,
    pub lat: f64,
}

pub type Row = BTreeMap<String, Value>;
