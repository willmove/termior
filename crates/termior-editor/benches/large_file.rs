use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use termior_editor::{EditorBuffer, SyntaxDocument, SyntaxLanguage};

fn five_megabyte_rust_source() -> String {
    let line = "pub fn generated_line() -> usize { 123456789 } // benchmark payload\n";
    line.repeat((5 * 1024 * 1024) / line.len() + 1)
}

fn large_file_benchmarks(criterion: &mut Criterion) {
    let source = five_megabyte_rust_source();
    let buffer = EditorBuffer::new(&source);
    let middle = buffer.len_lines() / 2;

    criterion.bench_function("editor/5mb_rope_open", |bench| {
        bench.iter(|| EditorBuffer::new(black_box(&source)));
    });

    criterion.bench_function("editor/5mb_visible_80_lines", |bench| {
        bench.iter(|| black_box(buffer.text_for_lines(middle..middle + 80)));
    });

    criterion.bench_function("editor/5mb_incremental_parse_edit", |bench| {
        bench.iter_batched(
            || {
                let mut buffer = EditorBuffer::new(&source);
                let syntax = SyntaxDocument::new(SyntaxLanguage::Rust, &source);
                let cursor = buffer.char_for_line_col(middle, 0);
                buffer.set_cursor(cursor).expect("middle cursor is valid");
                (buffer, syntax)
            },
            |(mut buffer, mut syntax)| {
                buffer
                    .insert("let benchmark_edit = 1;\n")
                    .expect("benchmark edit is valid");
                syntax.reparse_incremental(&buffer.text(), &buffer.take_pending_edits());
                black_box(syntax.has_error());
            },
            BatchSize::LargeInput,
        );
    });
}

criterion_group!(benches, large_file_benchmarks);
criterion_main!(benches);
