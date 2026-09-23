cargo build --release 
for x in 1004..1057 { ./target/release/crust-render -i samples/ALab/entry.usda -f $x -o $"renders/alab/seq/entry.($x).exr" }
