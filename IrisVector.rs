/* 
* Implementation of IRIS $VECTOR data structure that supports integer, decimal, and double
* IrisVector uses a region-based storage approach with bitmap tracking
* 
* Potential TODOs:
* - Add getter methods for integer and double types just like we started to with get_decimal
* - Add methods to query the bitmap state
* - Add methods to clear/remove values
*
* Written by: Nana Adjekum and William Goldman
* 12/27/2024 - 1/17/2025
*/

use std::convert::TryFrom;

// Type constants for the different vector types
const VEC_TYPE_INT: u8 = 0;
const VEC_TYPE_DECIMAL: u8 = 1;
const VEC_TYPE_DOUBLE: u8 = 2;

// Configuration constants
const VECTOR_MAGIC: u16 = 0x8B00;
const VEC_NUM_REGIONS: usize = 64;
const REGION_SIZE: usize = 1024;
const BITMAP_SIZE_BYTES: usize = 128;

// Macros for type checking and manipulation
macro_rules! get_vector_type {
    ($header:expr) => {
        ($header.type_field & 0xFF) as u8    // Extract type from header field
    };
}

macro_rules! is_type {
    ($header:expr, $type:expr) => {
        get_vector_type!($header) == $type   // Check if header matches specified type
    };
}

macro_rules! set_vector_type {
    ($header:expr, $type:expr) => {
        $header.type_field = VECTOR_MAGIC | ($type as u16)  // Set type in header with magic number
    };
}

macro_rules! has_type {
    ($header:expr) => {
        ($header.type_field & VECTOR_MAGIC) == VECTOR_MAGIC  // Check if header has valid type
    };
}

// Header structure with vector metadata
#[repr(C)]
struct VectorHeader {
    type_field: u16,
    format: u8,
    version: u8,
    first: u16,
    last: u16,
    count: u32,
    dic_offset: u32,
    dic_length: u32,
    refs: u16,
    uflags: u16,
    vflags: u16,
    stats: [u16; 19],
    regmap: [u16; VEC_NUM_REGIONS],
}

// Region structure that holds the actual data and bitmap
#[repr(C)]
struct DenseRegion<T> {
    bitmap: [u8; BITMAP_SIZE_BYTES],
    data: [T; REGION_SIZE]
}

// Main vector structure
pub struct Vector {
    header: VectorHeader,
    regions: Vec<Option<VectorRegion>>,
}

// Enum representing different types of regions
enum VectorRegion {
    Integer(Box<DenseRegion<i64>>),         // 64-bit integer storage
    Decimal(Box<DenseRegion<[u8; 9]>>),     // Fixed-point decimal storage (9 bytes)
    Double(Box<DenseRegion<f64>>),          // 64-bit floating point storage
}

// Sets a bit in the bitmap to track element presence
fn set_bitmap_bit(bitmap: &mut [u8; BITMAP_SIZE_BYTES], offset: usize, value: bool) {
    let byte_index = offset / 8;
    let bit_index = offset % 8;
    if value {
        bitmap[byte_index] |= 1 << bit_index;
    } else {
        bitmap[byte_index] &= !(1 << bit_index);
    }
}

// Converts a floating point number to fixed-point representation
fn convert_to_fixed_point(value: f64, scale: u32) -> Result<[u8; 9], &'static str> {
    let mut result = [0u8; 9];
    let scaled_value = (value * (10_f64.powi(scale as i32))) as i64;     // Scale the value and convert to integer
    result[0] = if scaled_value < 0 { 1 } else { 0 };     // Store sign in first byte
    
    // Store magnitude/value in remaining bytes
    let abs_value = scaled_value.abs();
    for i in 0..8 {
        result[8-i] = (abs_value >> (i * 8)) as u8;
    }
    
    Ok(result)
}

// Converts fixed-point representation back to floating point
fn convert_from_fixed_point(bytes: &[u8; 9], scale: u32) -> f64 {
    let sign = if bytes[0] == 0 { 1.0 } else { -1.0 };
    let mut value: i64 = 0;
    
    // Reconstruct value from bytes
    for i in 1..9 {
        value = (value << 8) | bytes[i] as i64;
    }

    sign * (value as f64) / 10_f64.powi(scale as i32)    // Apply scaling and sign
}

impl Vector {
    // New empty vector
    pub fn new() -> Self {
        let header = VectorHeader {
            type_field: 0,
            format: 0,
            version: 0,
            first: 0,
            last: 0,
            count: 0,
            dic_offset: 0,
            dic_length: 0,
            refs: 0,
            uflags: 0,
            vflags: 1,
            stats: [0; 19],
            regmap: [0; VEC_NUM_REGIONS],
        };

        Vector {
            header,
            regions: Vec::new(),
        }
    }

    // Converts type string to type constant
    fn type_from_str(type_str: &str) -> Option<u8> {
        match type_str.to_lowercase().as_str() {
            "integer" => Some(VEC_TYPE_INT),
            "decimal" => Some(VEC_TYPE_DECIMAL),
            "double" => Some(VEC_TYPE_DOUBLE),
            _ => None,
        }
    }

    // Set a value in the vector at the specified index
    pub fn set<T>(&mut self, index: usize, type_str: &str, value: T) -> Result<(), &'static str>
    where
        T: Copy + Into<f64>,  // The value must be convertible to f64 for type flexibility
    {
        let vector_type = Self::type_from_str(type_str)
            .ok_or("Invalid vector type specified")?;

        // Initialize or verify type
        if !has_type!(self.header) {
            set_vector_type!(self.header, vector_type);
        } else if !is_type!(self.header, vector_type) {
            return Err("Cannot change vector type after initialization");
        }

        self.set_value(index, value)
    }

    // Set value after type checking
    fn set_value<T>(&mut self, index: usize, value: T) -> Result<(), &'static str>
    where
        T: Copy + Into<f64>,
    {
        let (region_index, offset) = self.get_region_info(index);
        self.ensure_region(region_index)?;

        let float_val: f64 = value.into();

        match get_vector_type!(self.header) {
            VEC_TYPE_INT => {
                if let Some(VectorRegion::Integer(region)) = &mut self.regions[region_index] {
                    region.data[offset] = float_val as i64;
                    set_bitmap_bit(&mut region.bitmap, offset, true);
                }
            },
            VEC_TYPE_DECIMAL => {
                if let Some(VectorRegion::Decimal(region)) = &mut self.regions[region_index] {
                    let decimal_bytes = convert_to_fixed_point(float_val, 4)?;
                    region.data[offset] = decimal_bytes;
                    set_bitmap_bit(&mut region.bitmap, offset, true);
                }
            },
            VEC_TYPE_DOUBLE => {
                if let Some(VectorRegion::Double(region)) = &mut self.regions[region_index] {
                    region.data[offset] = float_val;
                    set_bitmap_bit(&mut region.bitmap, offset, true);
                }
            },
            _ => return Err("Unsupported type operation"),
        }

        self.update_header_for_set(index);
        Ok(())
    }

    // Ensure the region actually exists and that it is initialized
    fn ensure_region(&mut self, region_index: usize) -> Result<(), &'static str> {
        while self.regions.len() <= region_index {
            self.regions.push(None);
        }

        if self.regions[region_index].is_none() {
            let new_region = match get_vector_type!(self.header) {
                VEC_TYPE_INT => {
                    Some(VectorRegion::Integer(Box::new(DenseRegion {
                        bitmap: [0; BITMAP_SIZE_BYTES],
                        data: [0; REGION_SIZE],
                    })))
                },
                VEC_TYPE_DECIMAL => {
                    Some(VectorRegion::Decimal(Box::new(DenseRegion {
                        bitmap: [0; BITMAP_SIZE_BYTES],
                        data: [[0; 9]; REGION_SIZE],
                    })))
                },
                VEC_TYPE_DOUBLE => {
                    Some(VectorRegion::Double(Box::new(DenseRegion {
                        bitmap: [0; BITMAP_SIZE_BYTES],
                        data: [0.0; REGION_SIZE],
                    })))
                },
                _ => return Err("Unsupported vector type"),
            };
            
            self.regions[region_index] = new_region;
            self.header.regmap[region_index] = 1;
        }
        Ok(())
    }

    // Calculate the region index and offset for a given vector index
    fn get_region_info(&self, index: usize) -> (usize, usize) {
        (index / REGION_SIZE, index % REGION_SIZE)
    }

    // Update the header metadata when setting a value
    fn update_header_for_set(&mut self, index: usize) {
        self.header.count += 1;
        self.header.first = self.header.first.min(index as u16);
        self.header.last = self.header.last.max(index as u16);
    }

    // Only getter we implemented: retrieves a decimal value from the vector
    pub fn get_decimal(&self, index: usize) -> Result<f64, &'static str> {
        let (region_index, offset) = self.get_region_info(index);
        
        if let Some(VectorRegion::Decimal(region)) = &self.regions[region_index] {
            let byte_index = offset / 8;
            let bit_index = offset % 8;
            
            if (region.bitmap[byte_index] & (1 << bit_index)) != 0 {
                Ok(convert_from_fixed_point(&region.data[offset], 4))
            } else {
                Err("No value set at this index")
            }
        } else {
            Err("Not a decimal vector or region not initialized")
        }
    }

    /* Other getters here! */
}

// Example usage for all three types we implemented
fn example() -> Result<(), &'static str> {
    println!("\nCreating vectors...");

    let mut int_vector = Vector::new();
    int_vector.set(0, "integer", 42)?;
    int_vector.set(1, "integer", -17)?;
    int_vector.set(5, "integer", 1000)?;
    println!("Created integer vector with values at indices 0, 1, and 5");

    let mut double_vector = Vector::new();
    double_vector.set(0, "double", 3.14)?;
    double_vector.set(2, "double", -2.718)?;
    double_vector.set(4, "double", 1.618)?;
    println!("Created double vector with values at indices 0, 2, and 4");
    
    let mut decimal_vector = Vector::new();
    decimal_vector.set(0, "decimal", 123.4567)?;
    decimal_vector.set(1, "decimal", -45.678)?;
    decimal_vector.set(2, "decimal", 0.0001)?;
    println!("Value at index 0: {}", decimal_vector.get_decimal(0)?);
    println!("Value at index 1: {}", decimal_vector.get_decimal(1)?);
    println!("Value at index 2: {}", decimal_vector.get_decimal(2)?);

    Ok(())
}

fn main() {
    if let Err(e) = example() {
        eprintln!("Error in example: {}", e);
    }
}
