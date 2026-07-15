//! Generates the deterministic AcroForm fixture used by the tile-rendering test.

use std::fs;
use std::path::PathBuf;

fn stream(dictionary: &str, bytes: &[u8]) -> Vec<u8> {
    let mut object = format!("<< {dictionary} /Length {} >>\nstream\n", bytes.len()).into_bytes();
    object.extend_from_slice(bytes);
    object.extend_from_slice(b"\nendstream");
    object
}

fn main() {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("test/tile-form.pdf"));
    let page = br#"q
0.92 0.95 1 rg 0 0 612 792 re f
Q
BT /Helv 24 Tf 72 690 Td (Tile form and annotation fixture) Tj ET
BT /Helv 13 Tf 72 650 Td (The field below is rendered through the form-fill environment.) Tj ET
"#;
    let field_appearance = br#"q
1 1 0.85 rg 0 0 240 45 re f
0.15 0.2 0.35 RG 2 w 1 1 238 43 re S
Q
BT /Helv 16 Tf 8 15 Td (FORM VALUE) Tj ET
"#;
    let square_appearance = br#"q
0 1 1 rg 0 0 150 50 re f
1 0 1 RG 4 w 2 2 146 46 re S
Q
"#;

    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] /Resources << /Font << /Helv 5 0 R >> >> >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [7 0 R 9 0 R] >>".to_vec(),
        stream("", page),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
        b"<< /Fields [7 0 R] /DR << /Font << /Helv 5 0 R >> >> /DA (/Helv 16 Tf 0 g) /NeedAppearances true >>".to_vec(),
        b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (tile_field) /V (FORM VALUE) /Rect [120 400 360 445] /P 3 0 R /F 4 /DA (/Helv 16 Tf 0 g) /AP << /N 8 0 R >> >>".to_vec(),
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 240 45] /Resources << /Font << /Helv 5 0 R >> >>",
            field_appearance,
        ),
        b"<< /Type /Annot /Subtype /Square /Rect [100 300 250 350] /C [1 0 1] /IC [0 1 1] /F 4 /AP << /N 10 0 R >> >>".to_vec(),
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 150 50]",
            square_appearance,
        ),
    ];

    let mut pdf = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());

    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }

    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");

    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );

    fs::write(output, pdf).expect("could not write tile form fixture");
}
