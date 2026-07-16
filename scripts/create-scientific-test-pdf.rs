//! Creates an original, deterministic scientific-paper fixture with
//! unannotated superscript citations. It uses only the Rust standard library.

use std::env;
use std::fs;
use std::path::PathBuf;

fn stream(bytes: &[u8]) -> Vec<u8> {
    let mut object = format!("<< /Length {} >>\nstream\n", bytes.len()).into_bytes();
    object.extend_from_slice(bytes);
    object.extend_from_slice(b"\nendstream");
    object
}

fn main() {
    let output = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tests/fixtures/scientific-unlinked.pdf"));

    let body_one = br#"BT /F1 24 Tf 54 720 Td (Synthetic Scientific Paper) Tj ET
BT /F1 13 Tf 54 660 Td (Prior work established the first result) Tj ET
BT /F1 7 Tf 15 Ts 292 660 Td (1) Tj ET
BT /F1 13 Tf 54 610 Td (Further evidence supports the second claim) Tj ET
BT /F1 7 Tf 15 Ts 309 610 Td (2) Tj ET
BT /F1 13 Tf 54 560 Td (Another experiment confirmed the finding) Tj ET
BT /F1 7 Tf 15 Ts 299 560 Td (3) Tj ET
"#;
    let body_two = br#"BT /F1 20 Tf 54 720 Td (Results) Tj ET
BT /F1 13 Tf 54 660 Td (The final analysis agrees with earlier reports) Tj ET
BT /F1 7 Tf 15 Ts 323 660 Td (4) Tj ET
BT /F1 13 Tf 54 610 Td (This page intentionally contains no PDF link annotations.) Tj ET
"#;
    let references = br#"BT /F1 20 Tf 54 730 Td (REFERENCES) Tj ET
BT /F1 11 Tf 54 685 Td (1. Ada Author. First useful paper title. Journal One. doi:10.1000/one) Tj ET
BT /F1 11 Tf 54 645 Td (2. Ben Writer. Second useful paper title. Journal Two. doi:10.1000/two) Tj ET
BT /F1 11 Tf 54 605 Td (3. Cora Scholar. Third useful paper title. Journal Three. 2021.) Tj ET
BT /F1 11 Tf 54 565 Td (4. Dan Researcher. Fourth useful paper title. Journal Four. 2022.) Tj ET
BT /F1 11 Tf 54 525 Td (5. Eve Scientist. Fifth useful paper title. Journal Five. 2020.) Tj ET
BT /F1 11 Tf 54 485 Td (6. Finn Analyst. Sixth useful paper title. Journal Six. 2019.) Tj ET
BT /F1 11 Tf 54 445 Td (7. Gia Expert. Seventh useful paper title. Journal Seven. 2018.) Tj ET
BT /F1 11 Tf 54 405 Td (8. Hugo Investigator. Eighth useful paper title. Journal Eight. 2017.) Tj ET
"#;

    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 3 /Kids [3 0 R 5 0 R 7 0 R] /Resources << /Font << /F1 9 0 R >> >> >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        stream(body_one),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>".to_vec(),
        stream(body_two),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R >>".to_vec(),
        stream(references),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
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

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).expect("could not create fixture directory");
    }
    fs::write(&output, pdf).expect("could not write fixture PDF");
    println!("wrote {}", output.display());
}
