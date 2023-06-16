use protobuf_codegen::Codegen;

fn main() {
    Codegen::new()
        .pure()
        .includes(["src"])
        .input("src/discovery/waku/waku_message.proto")
        .cargo_out_dir("protos")
        .run_from_script();
}
