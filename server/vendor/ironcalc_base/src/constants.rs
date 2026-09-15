#![deny(missing_docs)]

/// Default Excel column width (8.43 characters) rendered at 96 DPI.
pub(crate) const DEFAULT_COLUMN_WIDTH: f64 = 64.0;

/// Default Excel row height (15 points) rendered at 96 DPI.
pub(crate) const DEFAULT_ROW_HEIGHT: f64 = 20.0;

/// Maximum-digit width used by Excel's default Calibri 11 column-width formula at 96 DPI.
pub const COLUMN_WIDTH_FACTOR: f64 = 7.0;

/// A row height is stored by SpreadsheetML in points; CSS uses 96-DPI pixels.
pub const ROW_HEIGHT_FACTOR: f64 = 96.0 / 72.0;

/// Converts SpreadsheetML column-width units to pixels using Excel's documented
/// maximum-digit-width calculation (MDW=7 for the default 11-point workbook font).
/// The five-pixel cell padding is part of the file-format geometry and must not be
/// folded into a linear scale factor.
pub fn excel_column_width_to_pixels(width: f64) -> f64 {
    if width <= 0.0 {
        0.0
    } else if width < 1.0 {
        (width * (COLUMN_WIDTH_FACTOR + 5.0)).floor()
    } else {
        (width * COLUMN_WIDTH_FACTOR + 5.0).floor()
    }
}

/// Converts a pixel column width back to SpreadsheetML character-width units.
/// Values are quantized to Excel's 1/256-character storage precision.
pub fn pixels_to_excel_column_width(pixels: f64) -> f64 {
    if pixels <= 0.0 {
        return 0.0;
    }
    let width = if pixels < COLUMN_WIDTH_FACTOR + 5.0 {
        pixels / (COLUMN_WIDTH_FACTOR + 5.0)
    } else {
        (pixels - 5.0) / COLUMN_WIDTH_FACTOR
    };
    // Round upward so converting the stored 1/256 value back with Excel's floor
    // rule returns the exact requested pixel width instead of losing one pixel.
    (width * 256.0).ceil() / 256.0
}

/// Default window height in pixels
pub(crate) const DEFAULT_WINDOW_HEIGHT: i64 = 600;

/// Default window width in pixels
pub(crate) const DEFAULT_WINDOW_WIDTH: i64 = 800;

/// Maximum number of columns
pub(crate) const LAST_COLUMN: i32 = 16_384;

/// Maximum number of rows
pub(crate) const LAST_ROW: i32 = 1_048_576;

/// Excel uses 15 significant digits of precision for all numeric calculations.
pub(crate) const EXCEL_PRECISION: usize = 15;

/// 693_594 is computed as:
/// NaiveDate::from_ymd(1900, 1, 1).num_days_from_ce() - 2
/// The 2 days offset is because of Excel 1900 bug
pub(crate) const EXCEL_DATE_BASE: i32 = 693_594;

/// We do not support dates before 1899-12-31.
pub(crate) const MINIMUM_DATE_SERIAL_NUMBER: i32 = 1;

/// Excel can handle dates until the year 9999-12-31
/// 2958465 is the number of days from 1900-01-01 to 9999-12-31
pub(crate) const MAXIMUM_DATE_SERIAL_NUMBER: i32 = 2_958_465;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_constants() {
        assert_eq!(excel_column_width_to_pixels(8.43), DEFAULT_COLUMN_WIDTH);
        assert_eq!(excel_column_width_to_pixels(35.0), 250.0);
        assert_eq!(ROW_HEIGHT_FACTOR * 15.0, DEFAULT_ROW_HEIGHT);
        assert!((pixels_to_excel_column_width(250.0) - 35.0).abs() < 1.0 / 256.0);
    }
}
