//! Pure-Rust EMF (Enhanced Metafile) reading and writing, limited to the
//! record set needed for vector geometry conversion. Replaces the role
//! libUEMF plays in the C implementation.

pub mod emfplus;
mod reader;
mod records;
pub mod types;
mod writer;

pub use reader::EmfFile;
pub use records::{EmfHeader, EmfRecord};
pub use writer::write_emf;

/// Errors produced while parsing an EMF byte stream.
#[derive(Debug, thiserror::Error)]
pub enum EmfError {
    #[error("not an EMF file (bad signature or first record)")]
    NotEmf,
    #[error("truncated or out-of-bounds data: {0}")]
    Truncated(&'static str),
    #[error("record type {itype} has invalid size {nsize}")]
    BadRecordSize { itype: u32, nsize: usize },
    #[error("malformed record: {0}")]
    Malformed(&'static str),
}

#[cfg(test)]
mod tests {
    use super::types::*;
    use super::*;

    fn roundtrip(rec: EmfRecord) {
        let header = EmfHeader {
            bounds: RectL::new(0, 0, 100, 100),
            frame: RectL::new(0, 0, 2646, 2646),
            ..Default::default()
        };
        let bytes = write_emf(&header, &[rec.clone(), EmfRecord::Eof]);
        let parsed = EmfFile::parse(&bytes).expect("parse back");
        assert_eq!(parsed.records.len(), 2);
        assert_eq!(parsed.records[0], rec);
        assert_eq!(parsed.records[1], EmfRecord::Eof);
        assert_eq!(parsed.header.bounds, header.bounds);
    }

    #[test]
    fn write_read_symmetry() {
        roundtrip(EmfRecord::SetMapMode(MM_ANISOTROPIC));
        roundtrip(EmfRecord::SetWindowExtEx(SizeL::new(1000, 2000)));
        roundtrip(EmfRecord::SetViewportOrgEx(PointL::new(-3, 7)));
        roundtrip(EmfRecord::MoveToEx(PointL::new(10, -20)));
        roundtrip(EmfRecord::SetWorldTransform(Xform {
            m11: 1.5,
            m12: 0.25,
            m21: -0.5,
            m22: 2.0,
            dx: 10.0,
            dy: -4.0,
        }));
        roundtrip(EmfRecord::ModifyWorldTransform {
            xform: Xform::IDENTITY,
            mode: MWT_LEFTMULTIPLY,
        });
        roundtrip(EmfRecord::CreatePen {
            ih: 1,
            style: PS_SOLID,
            width: 3,
            color: ColorRef::new(255, 0, 127),
        });
        roundtrip(EmfRecord::ExtCreatePen {
            ih: 2,
            style: PS_GEOMETRIC | PS_USERSTYLE | PS_ENDCAP_FLAT | PS_JOIN_BEVEL,
            width: 12,
            brush_style: BS_SOLID,
            color: ColorRef::new(1, 2, 3),
            hatch: 0,
            style_entries: vec![36, 12],
        });
        roundtrip(EmfRecord::CreateBrushIndirect {
            ih: 3,
            style: BS_SOLID,
            color: ColorRef::new(9, 8, 7),
            hatch: 0,
        });
        roundtrip(EmfRecord::SelectObject(STOCK_OBJECT_FLAG | NULL_PEN));
        roundtrip(EmfRecord::Polyline(vec![
            PointL::new(0, 0),
            PointL::new(50, 60),
            PointL::new(-10, 30),
        ]));
        roundtrip(EmfRecord::Polypolygon {
            counts: vec![3, 4],
            pts: vec![
                PointL::new(0, 0),
                PointL::new(10, 0),
                PointL::new(10, 10),
                PointL::new(20, 20),
                PointL::new(30, 20),
                PointL::new(30, 30),
                PointL::new(20, 30),
            ],
        });
        roundtrip(EmfRecord::PolyDraw {
            pts: vec![PointL::new(0, 0), PointL::new(5, 5), PointL::new(9, 1)],
            types: vec![PT_MOVETO, PT_LINETO, PT_LINETO | PT_CLOSEFIGURE],
        });
        roundtrip(EmfRecord::AngleArc {
            center: PointL::new(50, 50),
            radius: 25,
            start_angle: 0.0,
            sweep_angle: 270.0,
        });
        roundtrip(EmfRecord::RoundRect {
            rect: RectL::new(0, 0, 40, 20),
            corner: SizeL::new(8, 8),
        });
        roundtrip(EmfRecord::Pie {
            rect: RectL::new(0, 0, 100, 50),
            start: PointL::new(100, 25),
            end: PointL::new(0, 25),
        });
        roundtrip(EmfRecord::BeginPath);
        roundtrip(EmfRecord::FillPath(RectL::new(0, 0, 10, 10)));
        roundtrip(EmfRecord::omni_comment(
            OMNI_KIND_SVG,
            b"<svg>lossless</svg>",
        ));
        roundtrip(EmfRecord::Comment(vec![1, 2, 3])); // non-multiple-of-4 padded
        roundtrip(EmfRecord::Unknown {
            itype: 76, // EMR_BITBLT
            data: vec![0u8; 92],
        });
    }

    #[test]
    fn rejects_garbage() {
        assert!(EmfFile::parse(&[]).is_err());
        assert!(EmfFile::parse(&[0u8; 200]).is_err());
        let header = EmfHeader::default();
        let mut ok = write_emf(&header, &[EmfRecord::Eof]);
        // Corrupt the signature.
        ok[40] = 0;
        assert!(EmfFile::parse(&ok).is_err());
    }

    #[test]
    fn truncated_record_is_error() {
        let header = EmfHeader::default();
        let bytes = write_emf(
            &header,
            &[
                EmfRecord::Polyline(vec![PointL::new(0, 0), PointL::new(1, 1)]),
                EmfRecord::Eof,
            ],
        );
        // Cut the file in the middle of the polyline record.
        assert!(EmfFile::parse(&bytes[..100]).is_err());
    }
}
