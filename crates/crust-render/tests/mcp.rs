//! `crust mcp` end to end, driven over its pipes as Claude Desktop drives
//! it: one JSON-RPC message per line on stdin, and nothing but protocol
//! messages on stdout, whatever the log level.
#![cfg(feature = "mcp")]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples")
}

/// A fresh, empty directory for one test.
fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_mcp_cli").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `samples/cornellbox.usda` and the sky it reads, copied into `dir` as
/// `name`.
fn cornellbox_in(dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("dir");
    let stage = dir.join(name);
    std::fs::copy(samples().join("cornellbox.usda"), &stage).expect("copy the stage");
    std::fs::copy(
        samples().join("sky_gradient.exr"),
        dir.join("sky_gradient.exr"),
    )
    .expect("copy the sky");
    stage
}

/// Every file under `dir` with its bytes, to check nothing was written.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("list").flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                files.push((p.clone(), std::fs::read(&p).expect("read")));
            }
        }
    }
    files.sort();
    files
}

/// A client on a running `crust mcp`: requests go out one at a time, and
/// each call waits for its own reply, so a slow tool (an import) is never
/// cut short by stdin closing.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next: u64,
    /// Every message read so far, replies and notifications, in order.
    seen: Vec<Value>,
}

impl Client {
    fn start(args: &[&str]) -> Client {
        Client::start_in(Path::new(env!("CARGO_MANIFEST_DIR")), args)
    }

    /// [`start`](Self::start), the server's working directory `dir`.
    fn start_in(dir: &Path, args: &[&str]) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_crust"))
            .current_dir(dir)
            .arg("mcp")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run crust mcp");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut client = Client {
            child,
            stdin,
            stdout,
            next: 1,
            seen: Vec::new(),
        };
        let init = client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "crust-test", "version": "0" }
            }),
        );
        assert!(init.get("result").is_some(), "{init:#}");
        client.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        client
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{message}").expect("write a message");
        stdin.flush().expect("flush");
    }

    /// Sends a request and reads until its reply; every stdout line must be
    /// a JSON-RPC message.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("read stdout");
            assert!(
                n > 0,
                "the server closed stdout before replying to {method}"
            );
            let m: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line:?}"));
            assert_eq!(m["jsonrpc"], "2.0", "not a JSON-RPC message: {line}");
            self.seen.push(m.clone());
            if m["id"] == id {
                return m;
            }
        }
    }

    /// Calls `tool`: `Ok` with its structured result, `Err` with the text of
    /// a tool error.
    fn call(&mut self, tool: &str, arguments: Value) -> Result<Value, String> {
        let reply = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        let result = reply
            .get("result")
            .unwrap_or_else(|| panic!("{tool}: a protocol error {reply:#}"));
        if result["isError"] == true {
            Err(result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned())
        } else {
            Ok(result["structuredContent"].clone())
        }
    }

    /// Closes stdin and waits for the server to exit.
    fn finish(mut self) -> Output {
        drop(self.stdin.take());
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut self.stdout, &mut rest).expect("read the rest");
        for line in rest.lines() {
            let m: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line:?}"));
            assert_eq!(m["jsonrpc"], "2.0", "not a JSON-RPC message: {line}");
        }
        let mut out = self.child.wait_with_output().expect("crust mcp exits");
        out.stdout = rest.into_bytes();
        out
    }
}

const TOOLS: &[&str] = &[
    "open_session",
    "query",
    "check",
    "set_attribute",
    "set_variant",
    "set_active",
    "bind_material",
    "author_usda",
    "undo",
    "render",
    "snapshot",
    "cancel",
    "probe",
    "diff",
    "render_final",
];

#[test]
fn initialises_and_lists_tools_with_only_protocol_on_stdout() {
    let mut client = Client::start(&["-l", "debug"]);
    let listed = client.request("tools/list", json!({}));
    let tools = listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{listed:#}"))
        .clone();
    for tool in &tools {
        assert!(tool["name"].is_string(), "{tool:#}");
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool:#}");
    }
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for tool in TOOLS {
        assert!(names.contains(tool), "{tool} missing from {names:?}");
    }
    let init = &client.seen[0]["result"];
    assert_eq!(init["serverInfo"]["name"], "crust", "{init:#}");
    assert!(init["capabilities"]["tools"].is_object(), "{init:#}");

    let out = client.finish();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit {:?}\n{stderr}", out.status);
    // The log went to stderr: at debug, it saw the server stop.
    assert!(stderr.contains("MCP server stopped"), "{stderr}");
}

#[test]
fn closing_stdin_before_initialising_exits_without_writing() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_crust"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run crust mcp");
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("exits");
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn mcp_takes_no_render_flags() {
    let out = Command::new(env!("CARGO_BIN_EXE_crust"))
        .args(["mcp", "-i", "scene.usda"])
        .stdin(Stdio::null())
        .output()
        .expect("run crust");
    assert_eq!(out.status.code(), Some(2), "a usage error");
}

/// The prim opinions a layer's text holds: its `def`, `over` and `class`
/// lines.
fn prim_opinions(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim_start)
        .filter(|l| l.starts_with("def ") || l.starts_with("over ") || l.starts_with("class "))
        .collect()
}

#[test]
fn a_new_session_writes_a_layer_that_only_sublayers_its_input() {
    let dir = work_dir("new_session");
    let input = cornellbox_in(&dir, "shot.usda");
    let before = snapshot(&dir);
    let output = dir.join("shot_lookdev.usda");

    let mut client = Client::start(&[]);
    let opened = client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");
    assert_eq!(opened["resumed"], false, "{opened:#}");
    assert_eq!(opened["camera"], "/scene/camera1", "{opened:#}");
    assert!(opened["resolution"].is_array(), "{opened:#}");
    assert!(opened["warnings"].is_u64(), "{opened:#}");
    let out = client.finish();
    assert!(out.status.success());
    // The import's warning went to stderr; stdout held only protocol
    // (`finish` and every call checked each line).
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("material.fallback_default"), "{stderr}");

    let layer = std::fs::read_to_string(&output).expect("the override layer exists");
    assert!(layer.contains("@./shot.usda@"), "{layer}");
    assert!(prim_opinions(&layer).is_empty(), "{layer}");
    // The input and its sky are untouched; the layer is the one new file.
    let mut after = snapshot(&dir);
    after.retain(|(p, _)| p != &output);
    assert_eq!(before, after);
}

#[test]
fn the_output_cannot_be_a_source_layer() {
    let dir = work_dir("source_layer");
    let base = cornellbox_in(&dir, "base.usda");
    let shot = dir.join("shot.usda");
    std::fs::write(
        &shot,
        "#usda 1.0\n(\n    subLayers = [\n        @./base.usda@\n    ]\n)\n",
    )
    .expect("write the shot");
    let before = snapshot(&dir);

    let mut client = Client::start(&[]);
    // The input itself.
    let e = client
        .call("open_session", json!({ "input": shot, "output": shot }))
        .expect_err("refused");
    assert!(e.contains("is the input"), "{e}");
    // A layer the input composes.
    let e = client
        .call("open_session", json!({ "input": shot, "output": base }))
        .expect_err("refused");
    assert!(e.contains("is not an override layer"), "{e}");
    // An output that is not a text layer.
    let e = client
        .call(
            "open_session",
            json!({ "input": shot, "output": dir.join("x.usdc") }),
        )
        .expect_err("refused");
    assert!(e.contains(".usda"), "{e}");
    client.finish();
    assert_eq!(before, snapshot(&dir), "nothing written");
}

#[test]
fn a_session_resumes_its_own_layer_and_refuses_another_inputs() {
    let dir = work_dir("resume");
    let input = cornellbox_in(&dir, "shot.usda");
    let other = cornellbox_in(&dir.join("other"), "shot.usda");
    let output = dir.join("work").join("shot_lookdev.usda");

    let mut client = Client::start(&[]);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");
    // Yesterday's work: an opinion in the layer, authored by hand here.
    let mut layer = std::fs::read_to_string(&output).expect("layer");
    assert!(layer.contains("@../shot.usda@"), "{layer}");
    layer.push_str("over \"scene\"\n{\n    over \"Sky\"\n    {\n        float inputs:exposure = -1\n    }\n}\n");
    std::fs::write(&output, &layer).expect("edit the layer");
    let before = snapshot(&dir);

    let resumed = client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("resumed");
    assert_eq!(resumed["resumed"], true, "{resumed:#}");
    let e = client
        .call("open_session", json!({ "input": other, "output": output }))
        .expect_err("another input's layer");
    assert!(e.contains("is not an override layer"), "{e}");
    client.finish();
    assert_eq!(
        before,
        snapshot(&dir),
        "resuming and refusing write nothing"
    );
}

#[test]
fn query_names_the_layer_an_opinion_comes_from() {
    let dir = work_dir("query");
    let input = cornellbox_in(&dir, "shot.usda");
    let output = dir.join("shot_lookdev.usda");
    let mut client = Client::start(&[]);
    let e = client
        .call("query", json!({ "path": "/scene/Sky" }))
        .expect_err("no session yet");
    assert!(e.contains("open_session"), "{e}");
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");

    let sky = client
        .call("query", json!({ "path": "/scene/Sky" }))
        .expect("the light");
    assert_eq!(sky["type"], "DomeLight", "{sky:#}");
    assert_eq!(sky["active"], true, "{sky:#}");
    let format = client
        .call(
            "query",
            json!({ "path": "/scene/Sky.inputs:texture:format" }),
        )
        .expect("an authored attribute");
    assert_eq!(format["value"], "latlong", "{format:#}");
    let source = format["source"].as_str().expect("a layer");
    assert!(source.ends_with("shot.usda"), "{format:#}");
    // Unauthored, it has no type here (openusd 0.7 has no schema
    // definitions), and the error says how to author it anyway.
    let e = client
        .call("query", json!({ "path": "/scene/Sky.inputs:exposure" }))
        .expect_err("no layer authors the exposure");
    assert!(e.contains("explicit `type`"), "{e}");
    let scene = client
        .call("query", json!({ "path": "/scene" }))
        .expect("the root");
    let children = scene["children"].as_array().expect("children");
    assert!(children.contains(&json!("Sky")), "{scene:#}");
    let e = client
        .call("query", json!({ "path": "/nowhere" }))
        .expect_err("no such prim");
    assert!(e.contains("no prim"), "{e}");
    client.finish();
}

/// `crust <args>` in `dir`, its stdout.
fn crust_cli(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run crust")
}

#[test]
fn check_is_the_clis_report_on_the_saved_layer() {
    let dir = work_dir("check");
    let input = cornellbox_in(&dir, "shot.usda");
    let output = dir.join("shot_lookdev.usda");
    let mut client = Client::start(&[]);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");
    let mut session = client.call("check", json!({})).expect("the report");
    client.finish();

    let cli = crust_cli(
        &dir,
        &["check", "-i", output.to_str().unwrap(), "--json", "-"],
    );
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let mut cli: Value = serde_json::from_slice(&cli.stdout).expect("crust-check/1");
    assert_eq!(session["format"], "crust-check/1");
    // Everything but the import's per-phase timings.
    for report in [&mut session, &mut cli] {
        report.as_object_mut().unwrap().remove("import");
    }
    assert_eq!(session, cli);
}

/// A session on a copy of cornellbox in a fresh `dir`: the client, the
/// input and the override layer.
fn cornellbox_session(name: &str, args: &[&str]) -> (Client, PathBuf, PathBuf) {
    let dir = work_dir(name);
    let input = cornellbox_in(&dir, "shot.usda");
    let output = dir.join("shot_lookdev.usda");
    let mut client = Client::start(args);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");
    (client, input, output)
}

/// The composed value at `attr` on a fresh stage opened on `layer`: what any
/// USD tool reading the file sees.
fn composed(layer: &Path, attr: &str) -> openusd::sdf::Value {
    let stage = openusd::usd::Stage::open(layer.to_str().unwrap()).expect("the layer opens");
    stage
        .attribute(attr)
        .expect("a path")
        .get::<openusd::sdf::Value>()
        .expect("readable")
        .unwrap_or_else(|| panic!("{attr} has no value in {}", layer.display()))
}

#[test]
fn set_attribute_authors_an_opinion_over_the_source() {
    let (mut client, input, output) = cornellbox_session("set_attribute", &[]);
    let source = std::fs::read(&input).expect("input");

    // Nothing declares the exposure: the type is needed, and given.
    let e = client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": -0.5 }),
        )
        .expect_err("no declared type");
    assert!(e.contains("type"), "{e}");
    let set = client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": -0.5, "type": "float" }),
        )
        .expect("set");
    assert_eq!(set["undo_depth"], 1, "{set:#}");
    assert!(set["import_s"].is_number(), "{set:#}");
    let layer = std::fs::read_to_string(&output).expect("layer");
    assert!(layer.contains("over \"scene\""), "{layer}");
    assert!(layer.contains("over \"Sky\""), "{layer}");
    assert!(layer.contains("float inputs:exposure = -0.5"), "{layer}");
    assert_eq!(
        std::fs::read(&input).unwrap(),
        source,
        "the source is unchanged"
    );

    // Now the layer declares it: the type comes from there, and the query
    // names the override layer as the source.
    let q = client
        .call("query", json!({ "path": "/scene/Sky.inputs:exposure" }))
        .expect("query");
    assert_eq!(q["value"], -0.5, "{q:#}");
    assert!(
        q["source"].as_str().unwrap().ends_with("shot_lookdev.usda"),
        "{q:#}"
    );
    client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": 1 }),
        )
        .expect("set again");
    // A value of the wrong type names the declared type, and authors nothing.
    let before = std::fs::read(&output).unwrap();
    let e = client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": "bright" }),
        )
        .expect_err("not a float");
    assert!(e.contains("float"), "{e}");
    assert_eq!(
        std::fs::read(&output).unwrap(),
        before,
        "the layer is unchanged"
    );
    // An attribute whose opinion lives in the source.
    client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:texture:format", "value": "angular" }),
        )
        .expect("a token");
    client.finish();
    assert_eq!(
        std::fs::read(&input).unwrap(),
        source,
        "the source is unchanged"
    );

    // What any USD tool composes from the file.
    assert_eq!(
        composed(&output, "/scene/Sky.inputs:exposure"),
        openusd::sdf::Value::Float(1.0)
    );
    assert_eq!(
        composed(&output, "/scene/Sky.inputs:texture:format"),
        openusd::sdf::Value::Token("angular".into())
    );
}

/// A stage with a variant set and a material to bind, in `dir`.
fn variant_stage(dir: &Path) -> PathBuf {
    let stage = dir.join("asset.usda");
    std::fs::write(
        &stage,
        r#"#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World"
{
    def Camera "cam"
    {
        double3 xformOp:translate = (0, 0, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def SphereLight "key"
    {
        float inputs:intensity = 10
        float inputs:radius = 0.5
        double3 xformOp:translate = (3, 3, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def Xform "asset" (
        variants = {
            string lod = "low"
        }
        prepend variantSets = "lod"
    )
    {
        variantSet "lod" = {
            "high" {
                def Sphere "ball"
                {
                    double radius = 2
                }
            }
            "low" {
                def Sphere "ball"
                {
                    double radius = 1
                }
            }
        }
    }

    def Scope "Looks"
    {
        def Material "red"
        {
        }
    }
}
"#,
    )
    .expect("write the stage");
    stage
}

#[test]
fn variants_activity_and_bindings_are_opinions_too() {
    let dir = work_dir("variants");
    let input = variant_stage(&dir);
    let source = std::fs::read(&input).unwrap();
    let output = dir.join("asset_lookdev.usda");
    let mut client = Client::start(&[]);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");

    let asset = client
        .call("query", json!({ "path": "/World/asset" }))
        .expect("the asset");
    assert_eq!(
        asset["variant_sets"]["lod"]["selection"], "low",
        "{asset:#}"
    );
    let e = client
        .call(
            "set_variant",
            json!({ "prim": "/World/asset", "variant_set": "lod", "variant": "huge" }),
        )
        .expect_err("no such variant");
    assert!(e.contains("high"), "{e}");
    client
        .call(
            "set_variant",
            json!({ "prim": "/World/asset", "variant_set": "lod", "variant": "high" }),
        )
        .expect("selected");
    let ball = client
        .call("query", json!({ "path": "/World/asset/ball.radius" }))
        .expect("the ball");
    assert_eq!(ball["value"], 2.0, "the high variant composes: {ball:#}");

    client
        .call(
            "bind_material",
            json!({ "prim": "/World/asset/ball", "material": "/World/Looks/red" }),
        )
        .expect("bound");
    let e = client
        .call(
            "bind_material",
            json!({ "prim": "/World/asset/ball", "material": "/World/cam" }),
        )
        .expect_err("not a material");
    assert!(e.contains("Material"), "{e}");
    client
        .call(
            "set_active",
            json!({ "prim": "/World/key", "active": false }),
        )
        .expect("deactivated");
    let key = client
        .call("query", json!({ "path": "/World/key" }))
        .expect("still queryable");
    assert_eq!(key["active"], false, "{key:#}");
    client.finish();
    assert_eq!(
        std::fs::read(&input).unwrap(),
        source,
        "the source is unchanged"
    );

    // Round trip: a fresh stage on the file composes every opinion.
    let stage = openusd::usd::Stage::open(output.to_str().unwrap()).expect("opens");
    let selections = stage
        .prim("/World/asset")
        .unwrap()
        .variant_sets()
        .get_all_variant_selections()
        .unwrap();
    assert!(
        selections.contains(&("lod".into(), "high".into())),
        "{selections:?}"
    );
    assert_eq!(
        composed(&output, "/World/asset/ball.radius"),
        openusd::sdf::Value::Double(2.0)
    );
    let binding = stage
        .relationship("/World/asset/ball.material:binding")
        .unwrap()
        .targets()
        .unwrap();
    assert_eq!(
        binding,
        vec![openusd::sdf::Path::new("/World/Looks/red").unwrap()]
    );
    assert!(!stage.prim("/World/key").unwrap().is_active().unwrap());
}

#[test]
fn author_usda_is_one_batch_and_refusals_leave_the_layer_alone() {
    let (mut client, _input, output) = cornellbox_session("author_usda", &["-l", "debug"]);
    // Three opinions: two inputs on the sky, and a new light.
    let done = client
        .call(
            "author_usda",
            json!({ "text": r#"
over "scene"
{
    over "Sky"
    {
        float inputs:exposure = -1
        color3f inputs:color = (1, 0.9, 0.8)
    }

    def RectLight "card"
    {
        float inputs:width = 0
        float inputs:height = 1
    }
}
"# }),
        )
        .expect("authored");
    // A zero-width rect light is skipped, and the result says so.
    let gained = done["warnings_gained"].as_array().expect("codes").clone();
    assert!(
        gained.contains(&json!("light.degenerate_shape")),
        "{done:#}"
    );
    let layer = std::fs::read_to_string(&output).unwrap();
    assert!(
        layer.contains("def RectLight \"card\""),
        "the file already holds it: {layer}"
    );

    let before = std::fs::read(&output).unwrap();
    let e = client
        .call(
            "author_usda",
            json!({ "text": "#usda 1.0\n(\n    subLayers = [@./other.usda@]\n)\n" }),
        )
        .expect_err("a sublayer");
    assert!(e.contains("subLayers"), "{e}");
    let e = client
        .call("author_usda", json!({ "text": "over \"scene\" {" }))
        .expect_err("does not parse");
    assert!(e.contains("parse"), "{e}");
    assert_eq!(std::fs::read(&output).unwrap(), before, "byte-identical");
    let out = client.finish();
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The open's import, then the one batch.
    assert_eq!(stderr.matches("Session import of").count(), 2, "{stderr}");
}

#[test]
fn undo_restores_the_layer_byte_for_byte() {
    let (mut client, input, output) = cornellbox_session("undo", &[]);
    let e = client.call("undo", json!({})).expect_err("nothing yet");
    assert!(e.contains("nothing to undo"), "{e}");
    let before = std::fs::read(&output).unwrap();
    client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": -0.5, "type": "float" }),
        )
        .expect("set");
    let middle = std::fs::read(&output).unwrap();
    client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": 2, "type": "float" }),
        )
        .expect("set again");
    let undone = client.call("undo", json!({})).expect("undone");
    assert_eq!(undone["undo_depth"], 1, "{undone:#}");
    assert_eq!(std::fs::read(&output).unwrap(), middle);
    client.call("undo", json!({})).expect("undone again");
    assert_eq!(std::fs::read(&output).unwrap(), before);
    let q = client
        .call("query", json!({ "path": "/scene/Sky.inputs:exposure" }))
        .expect_err("the opinion is gone");
    assert!(q.contains("no layer authors"), "{q}");

    // A resumed session starts with no history.
    client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": -0.5, "type": "float" }),
        )
        .expect("set");
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("resumed");
    let e = client
        .call("undo", json!({}))
        .expect_err("an empty history");
    assert!(e.contains("nothing to undo"), "{e}");
    client.finish();
}

#[test]
fn asset_paths_follow_the_layer_to_another_directory() {
    let dir = work_dir("anchoring");
    let shots = dir.join("shots").join("a");
    let input = cornellbox_in(&shots, "shot.usda");
    std::fs::create_dir_all(shots.join("tex")).unwrap();
    std::fs::copy(
        samples().join("sky_gradient.exr"),
        shots.join("tex").join("wood.exr"),
    )
    .unwrap();
    let output = dir.join("work").join("shot_lookdev.usda");
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let mut client = Client::start(&[]);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");
    // As the agent sees the input's paths: relative to the input's directory.
    let set = client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:texture:file", "value": "./tex/wood.exr" }),
        )
        .expect("set");
    assert_eq!(
        set["warnings_gained"],
        json!([]),
        "the texture is found: {set:#}"
    );
    // Which a path that resolves nowhere would not be.
    let missing = client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:texture:file", "value": "./tex/none.exr" }),
        )
        .expect("set");
    assert_eq!(
        missing["warnings_gained"],
        json!(["light.map_unreadable"]),
        "{missing:#}"
    );
    client.call("undo", json!({})).expect("undone");
    client.finish();
    let layer = std::fs::read_to_string(&output).unwrap();
    assert!(layer.contains("@../shots/a/shot.usda@"), "{layer}");
    assert!(layer.contains("@../shots/a/tex/wood.exr@"), "{layer}");

    // A server started somewhere else resumes the layer, and the asset still
    // resolves to the file it named.
    let mut client = Client::start_in(&elsewhere, &[]);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("resumed");
    let file = client
        .call("query", json!({ "path": "/scene/Sky.inputs:texture:file" }))
        .expect("the file");
    let resolved = file["value"]["resolved"].as_str().expect("resolved");
    assert!(
        same_file(Path::new(resolved), &shots.join("tex").join("wood.exr")),
        "{file:#}"
    );
    client.finish();
}

/// Whether two paths name the same existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

/// The PNG a render answer carries, decoded.
fn answer_image(client: &Client) -> image::RgbaImage {
    let reply = client.seen.last().expect("a reply");
    let content = reply["result"]["content"].as_array().expect("content");
    let png = content
        .iter()
        .find(|c| c["type"] == "image")
        .unwrap_or_else(|| panic!("no image in {reply:#}"));
    assert_eq!(png["mimeType"], "image/png");
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png["data"].as_str().unwrap())
        .expect("base64");
    image::load_from_memory(&bytes).expect("a PNG").to_rgba8()
}

#[test]
fn a_long_render_returns_within_its_budget_and_keeps_refining() {
    let (mut client, _input, _output) = cornellbox_session("long_render", &[]);
    let asked = std::time::Instant::now();
    // Far more samples than a debug build traces in minutes. A small region,
    // so the answer's PNG costs nothing beside the budget: the render keeps
    // every core busy while the debug build encodes it.
    let first = client
        .call(
            "render",
            json!({ "region": [0, 0, 160, 90], "spp": 100000, "budget_s": 2 }),
        )
        .expect("started");
    let waited = asked.elapsed().as_secs_f64();
    assert!(waited < 2.0 + 1.0, "answered after {waited} s: {first:#}");
    assert_eq!(first["done"], false, "{first:#}");
    assert_eq!(first["status"], "rendering", "{first:#}");
    let id = first["render_id"].as_u64().expect("an id");
    std::thread::sleep(std::time::Duration::from_secs(2));
    let later = client
        .call("snapshot", json!({ "render_id": id }))
        .expect("a snapshot");
    assert!(
        later["generation"].as_u64() > first["generation"].as_u64(),
        "a newer image: {first:#}\n{later:#}"
    );
    assert!(
        later["spp_reached"].as_u64() >= first["spp_reached"].as_u64(),
        "{first:#}\n{later:#}"
    );
    answer_image(&client);

    // An edit cancels it before the import.
    let edit = client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": -0.5, "type": "float" }),
        )
        .expect("set");
    assert_eq!(edit["cancelled_render"], id, "{edit:#}");
    let after = client
        .call("snapshot", json!({ "render_id": id }))
        .expect("kept");
    assert_eq!(after["status"], "cancelled", "{after:#}");
    assert_eq!(after["done"], true, "{after:#}");

    // So does `cancel`, keeping what it traced.
    let second = client
        .call("render", json!({ "spp": 100000, "budget_s": 1 }))
        .expect("started");
    let second_id = second["render_id"].as_u64().unwrap();
    assert_ne!(second_id, id);
    let cancelled = client
        .call("cancel", json!({ "render_id": second_id }))
        .expect("cancelled");
    assert_eq!(cancelled["status"], "cancelled", "{cancelled:#}");
    answer_image(&client);
    let e = client
        .call("snapshot", json!({ "render_id": 999 }))
        .expect_err("no such render");
    assert!(e.contains("no render 999"), "{e}");
    client.finish();
}

#[test]
fn a_region_renders_alone_and_a_render_completes() {
    let (mut client, _input, _output) = cornellbox_session("region_render", &[]);
    let done = client
        .call(
            "render",
            json!({ "region": [8, 16, 72, 48], "spp": 2, "budget_s": 120 }),
        )
        .expect("rendered");
    assert_eq!(done["done"], true, "{done:#}");
    assert_eq!(done["status"], "done", "{done:#}");
    assert_eq!(done["region"], json!([8, 16, 72, 48]), "{done:#}");
    assert_eq!(done["spp_reached"]["min"], 2, "{done:#}");
    let png = answer_image(&client);
    assert_eq!(png.dimensions(), (64, 32), "only the region");

    let e = client
        .call("render", json!({ "region": [8, 8, 8, 9] }))
        .expect_err("empty");
    assert!(e.contains("empty"), "{e}");
    let e = client
        .call("render", json!({ "budget_s": -1 }))
        .expect_err("a negative budget");
    assert!(e.contains("budget_s"), "{e}");
    client.finish();
}

#[test]
fn a_render_answers_at_its_first_image() {
    let (mut client, _input, _output) = cornellbox_session("first_image", &[]);
    // A render that cannot finish within its budget answers at 4 spp, long
    // before the budget.
    let asked = std::time::Instant::now();
    let first = client
        .call(
            "render",
            json!({ "region": [0, 0, 160, 90], "spp": 100000, "budget_s": 120 }),
        )
        .expect("started");
    let waited = asked.elapsed().as_secs_f64();
    assert!(waited < 60.0, "answered after {waited} s: {first:#}");
    assert_eq!(first["done"], false, "{first:#}");
    assert!(first["spp_reached"].as_u64() >= Some(4), "{first:#}");
    answer_image(&client);

    // `snapshot` with a budget waits for the finished image, without
    // restarting the render.
    let early = client
        .call(
            "render",
            json!({ "region": [8, 16, 72, 48], "spp": 16, "budget_s": 600 }),
        )
        .expect("started");
    let id = early["render_id"].as_u64().unwrap();
    assert!(
        early["done"] == true || early["spp_reached"].as_u64() >= Some(4),
        "{early:#}"
    );
    let finished = client
        .call("snapshot", json!({ "render_id": id, "budget_s": 600 }))
        .expect("waited");
    assert_eq!(finished["render_id"], id, "{finished:#}");
    assert_eq!(finished["status"], "done", "{finished:#}");
    assert_eq!(finished["spp_reached"]["min"], 16, "{finished:#}");
    answer_image(&client);

    let e = client
        .call("snapshot", json!({ "render_id": id, "budget_s": -1 }))
        .expect_err("a negative budget");
    assert!(e.contains("budget_s"), "{e}");
    let e = client
        .call("render", json!({ "wait": "forever" }))
        .expect_err("an unknown wait");
    assert!(e.contains("`image` or `done`"), "{e}");
    client.finish();
}

#[test]
fn probe_reads_the_pixel_the_cli_writes() {
    let (mut client, _input, output) = cornellbox_session("probe", &[]);
    let done = client
        .call(
            "render",
            json!({ "region": [256, 96, 352, 192], "spp": 16, "budget_s": 600, "wait": "done" }),
        )
        .expect("rendered");
    assert_eq!(done["status"], "done", "{done:#}");
    let id = done["render_id"].as_u64().unwrap();
    let (x, y) = (300usize, 150usize);
    let probed = client
        .call("probe", json!({ "render_id": id, "x": x, "y": y }))
        .expect("probed");
    assert!(probed["prim"].is_null(), "{probed:#}");
    assert!(probed["prim_note"].is_string(), "{probed:#}");
    let e = client
        .call("probe", json!({ "render_id": id, "x": 10, "y": 10 }))
        .expect_err("outside the region");
    assert!(e.contains("outside"), "{e}");
    client.finish();

    // The CLI's render of the same layer, region and samples.
    let dir = output.parent().unwrap();
    let cli = crust_cli(
        dir,
        &[
            "render",
            "-i",
            output.to_str().unwrap(),
            "-s",
            "16",
            "--region",
            "256,96,352,192",
            "-o",
            "probe.exr",
            "-l",
            "error",
        ],
    );
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let exr = crust_assets::read_exr_planes(&dir.join("probe.exr")).expect("the EXR");
    let i = (y - 96) * exr.width + (x - 256);
    for (k, c) in ["R", "G", "B"].iter().enumerate() {
        let theirs = exr.channels[*c].values[i];
        let ours = probed["beauty"][k].as_f64().unwrap() as f32;
        assert_eq!(ours.to_bits(), theirs.to_bits(), "{c}: {ours} vs {theirs}");
    }
}

#[test]
fn probe_answers_every_aov_of_the_stages_products() {
    let dir = work_dir("probe_aovs");
    let input = samples().join("aovs.usda");
    let output = dir.join("aovs_lookdev.usda");
    let mut client = Client::start(&[]);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");
    let done = client
        .call("render", json!({ "spp": 4, "budget_s": 600 }))
        .expect("rendered");
    let id = done["render_id"].as_u64().unwrap();
    let probed = client
        .call("probe", json!({ "render_id": id, "x": 80, "y": 45 }))
        .expect("probed");
    let aovs = probed["aovs"].as_object().expect("aovs");
    assert_eq!(aovs.len(), 2, "two products: {probed:#}");
    let all: Vec<&String> = aovs
        .values()
        .flat_map(|p| p.as_object().unwrap().keys())
        .collect();
    for channel in ["N.X", "P.X", "Z", "sampleCount"] {
        assert!(
            all.iter().any(|c| c.ends_with(channel)),
            "{channel} in {all:?}"
        );
    }
    client.finish();
}

#[test]
fn diff_compares_renders_and_refuses_mismatches() {
    let (mut client, _input, _output) = cornellbox_session("diff", &[]);
    let region = json!([256, 96, 352, 192]);
    let render = |client: &mut Client, spp: u32| {
        let r = client
            .call(
                "render",
                json!({ "region": region, "spp": spp, "budget_s": 600, "wait": "done" }),
            )
            .expect("rendered");
        assert_eq!(r["status"], "done", "{r:#}");
        r["render_id"].as_u64().unwrap()
    };
    let a = render(&mut client, 16);
    // An edit that changes nothing visible: the format the source already has.
    client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:texture:format", "value": "latlong" }),
        )
        .expect("set");
    let b = render(&mut client, 16);
    let same = client
        .call("diff", json!({ "render_a": a, "render_b": b }))
        .expect("compared");
    assert_eq!(same["identical"], true, "{same:#}");
    // A visible one.
    client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": 1, "type": "float" }),
        )
        .expect("set");
    let c = render(&mut client, 16);
    let changed = client
        .call("diff", json!({ "render_a": a, "render_b": c }))
        .expect("compared");
    assert_eq!(changed["identical"], false, "{changed:#}");
    let d = render(&mut client, 2);
    let e = client
        .call("diff", json!({ "render_a": a, "render_b": d }))
        .expect_err("different spp");
    assert!(e.contains("spp"), "{e}");
    client.finish();
}

#[test]
fn the_session_keeps_its_eight_latest_renders() {
    let (mut client, _input, _output) = cornellbox_session("retention", &[]);
    let mut ids = Vec::new();
    for _ in 0..9 {
        let r = client
            .call(
                "render",
                json!({ "region": [0, 0, 8, 8], "spp": 1, "budget_s": 600 }),
            )
            .expect("rendered");
        ids.push((r["render_id"].as_u64().unwrap(), r["evicted"].clone()));
    }
    for (_, evicted) in &ids[..8] {
        assert_eq!(evicted, &json!([]));
    }
    assert_eq!(ids[8].1, json!([ids[0].0]), "the oldest goes");
    let e = client
        .call("snapshot", json!({ "render_id": ids[0].0 }))
        .expect_err("evicted");
    assert!(e.contains("no render"), "{e}");
    client
        .call("snapshot", json!({ "render_id": ids[1].0 }))
        .expect("kept");
    client.finish();
}

/// Render settings small enough for a debug build, as opinions in the layer.
const SMALL_SETTINGS: &str = r#"
def Scope "Render"
{
    def RenderSettings "settings"
    {
        int crust:samplesPerPixel = 16
        int2 resolution = (96, 54)
    }
}
"#;

#[test]
fn render_final_writes_what_the_cli_writes_beside_the_layer() {
    let dir = work_dir("render_final");
    let input = cornellbox_in(&dir.join("shots"), "shot.usda");
    let work = dir.join("work");
    let output = work.join("x.usda");
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let mut client = Client::start(&[]);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");
    client
        .call("author_usda", json!({ "text": SMALL_SETTINGS }))
        .expect("settings");
    let done = client.call("render_final", json!({})).expect("rendered");
    client.finish();
    let files: Vec<PathBuf> = done["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| PathBuf::from(f.as_str().unwrap()))
        .collect();
    assert_eq!(
        files,
        vec![work.join("x.exr"), work.join("x.png")],
        "{done:#}"
    );

    // `crust render -i work/x.usda -o work/…` from another directory.
    let cli = crust_cli(
        &elsewhere,
        &[
            "render",
            "-i",
            output.to_str().unwrap(),
            "-o",
            work.join("cli.exr").to_str().unwrap(),
            "-l",
            "error",
        ],
    );
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let diff = crust_cli(&work, &["diff", "cli.exr", "x.exr"]);
    assert_eq!(
        diff.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&diff.stdout)
    );
    assert_eq!(
        std::fs::read(work.join("x.png")).unwrap(),
        std::fs::read(work.join("cli.png")).unwrap(),
        "the same PNG"
    );
}

#[test]
fn render_final_resolves_products_against_the_layers_directory() {
    let dir = work_dir("render_final_products");
    let input = samples().join("aovs.usda");
    let output = dir.join("work").join("aovs_lookdev.usda");
    let mut client = Client::start(&[]);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");
    // The output name is an opinion too: the beauty product moves.
    client
        .call(
            "set_attribute",
            json!({ "path": "/Render/Products/beauty.productName", "value": "final/beauty.exr" }),
        )
        .expect("renamed");
    let done = client.call("render_final", json!({})).expect("rendered");
    client.finish();
    let work = dir.join("work");
    for file in [
        work.join("final").join("beauty.exr"),
        work.join("final").join("beauty.png"),
        work.join("renders").join("aovs_data.exr"),
    ] {
        assert!(file.is_file(), "{} in {done:#}", file.display());
    }
    assert!(
        !samples().join("final").exists(),
        "nothing beside the input"
    );
}

#[test]
fn opening_another_stage_cancels_the_first_sessions_render() {
    let (mut client, _input, output) = cornellbox_session("switch_a", &[]);
    let render = client
        .call("render", json!({ "spp": 100000, "budget_s": 1 }))
        .expect("started");
    let layer = std::fs::read(&output).unwrap();
    let dir = work_dir("switch_b");
    let other = cornellbox_in(&dir, "other.usda");
    let opened = client
        .call(
            "open_session",
            json!({ "input": other, "output": dir.join("other_lookdev.usda") }),
        )
        .expect("the second session");
    assert_eq!(
        opened["cancelled_render"], render["render_id"],
        "{opened:#}"
    );
    assert_eq!(
        std::fs::read(&output).unwrap(),
        layer,
        "the first layer stays"
    );
    client.finish();
}

/// The look-dev session of `site/content/docs/help/claude-desktop.md`, call
/// for call: the page's example must work as written.
#[test]
fn the_documented_look_dev_session_runs() {
    let dir = work_dir("worked_session");
    let input = cornellbox_in(&dir, "cornellbox.usda");
    let output = dir.join("lookdev").join("cornellbox_lookdev.usda");
    let mut client = Client::start(&[]);
    let opened = client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("open_session");
    assert_eq!(opened["camera"], "/scene/camera1", "{opened:#}");
    assert_eq!(opened["resolution"], json!([640, 360]), "{opened:#}");
    assert_eq!(
        opened["warning_codes"],
        json!(["material.fallback_default"]),
        "{opened:#}"
    );
    let sky = client
        .call("query", json!({ "path": "/scene/Sky" }))
        .expect("query");
    assert_eq!(sky["type"], "DomeLight", "{sky:#}");
    let file = client
        .call("query", json!({ "path": "/scene/Sky.inputs:texture:file" }))
        .expect("query the file");
    assert_eq!(file["value"]["authored"], "sky_gradient.exr", "{file:#}");
    let first = client
        .call(
            "render",
            json!({ "region": [160, 90, 480, 270], "spp": 16, "budget_s": 600, "wait": "done" }),
        )
        .expect("render");
    assert_eq!(first["status"], "done", "{first:#}");
    let set = client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": -1, "type": "float" }),
        )
        .expect("set_attribute");
    assert_eq!(set["warnings_gained"], json!([]), "{set:#}");
    let after_set = std::fs::read(&output).unwrap();
    let key = client
        .call(
            "author_usda",
            json!({ "text": r#"
over "scene" {
    def RectLight "key" {
        float inputs:intensity = 30
        float inputs:width = 1
        float inputs:height = 1
        double3 xformOp:translate = (0, 3.9, 0)
        float3 xformOp:rotateXYZ = (-90, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]
    }
}"# }),
        )
        .expect("author_usda");
    assert_eq!(key["warnings_gained"], json!([]), "{key:#}");
    let second = client
        .call(
            "render",
            json!({ "region": [160, 90, 480, 270], "spp": 16, "budget_s": 600, "wait": "done" }),
        )
        .expect("render");
    let diff = client
        .call(
            "diff",
            json!({ "render_a": first["render_id"], "render_b": second["render_id"] }),
        )
        .expect("diff");
    assert_eq!(diff["identical"], false, "{diff:#}");
    let probed = client
        .call(
            "probe",
            json!({ "render_id": second["render_id"], "x": 320, "y": 200 }),
        )
        .expect("probe");
    assert_eq!(
        probed["beauty"].as_array().map(Vec::len),
        Some(3),
        "{probed:#}"
    );
    client.call("undo", json!({})).expect("undo");
    assert_eq!(
        std::fs::read(&output).unwrap(),
        after_set,
        "the key light is gone"
    );
    // The final render at the stage's own 128 spp takes two minutes in a
    // debug build: there, and only there, the settings are made small first.
    // (`render_final` is pinned against the CLI by its own tests.)
    if cfg!(debug_assertions) {
        client
            .call("author_usda", json!({ "text": SMALL_SETTINGS }))
            .expect("small settings");
    }
    let done = client
        .call("render_final", json!({}))
        .expect("render_final");
    let files = done["files"].as_array().expect("files");
    assert!(
        files[0]
            .as_str()
            .unwrap()
            .ends_with("cornellbox_lookdev.exr"),
        "{done:#}"
    );
    client.finish();
}

/// The Claude Desktop bundle's manifest (`packaging/mcpb/manifest.json`)
/// declares exactly the tools the server lists, at the crate's version.
#[test]
fn the_desktop_bundle_manifest_matches_the_server() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/mcpb/manifest.json");
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the manifest")).expect("JSON");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["server"]["mcp_config"]["args"], json!(["mcp"]));
    let mut declared: Vec<&str> = manifest["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().expect("a name"))
        .collect();
    let mut client = Client::start(&[]);
    let listed = client.request("tools/list", json!({}));
    client.finish();
    let mut served: Vec<String> = listed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    declared.sort_unstable();
    served.sort_unstable();
    assert_eq!(declared, served);
    let mut spec: Vec<&str> = TOOLS.to_vec();
    spec.sort_unstable();
    assert_eq!(spec, served, "the spec's tool list");
}

#[test]
fn binding_a_material_keeps_the_api_schemas_the_layer_applies() {
    let dir = work_dir("api_schemas");
    let input = variant_stage(&dir);
    let output = dir.join("asset_lookdev.usda");
    let mut client = Client::start(&[]);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("opened");
    client
        .call(
            "author_usda",
            json!({ "text": r#"over "World" { over "key" ( prepend apiSchemas = ["ShadowAPI"] ) { } }"# }),
        )
        .expect("ShadowAPI");
    client
        .call(
            "author_usda",
            json!({ "text": r#"over "World" { over "key" ( prepend apiSchemas = ["ShapingAPI"] ) { } }"# }),
        )
        .expect("ShapingAPI");
    client.finish();
    let stage = openusd::usd::Stage::open(output.to_str().unwrap()).expect("opens");
    let schemas: Vec<String> = stage
        .prim("/World/key")
        .unwrap()
        .authored_api_schemas()
        .unwrap()
        .iter()
        .map(|t| t.as_str().to_owned())
        .collect();
    assert!(
        schemas.contains(&"ShadowAPI".to_owned()) && schemas.contains(&"ShapingAPI".to_owned()),
        "{schemas:?}"
    );

    // And bind_material, whose snippet prepends MaterialBindingAPI.
    let mut client = Client::start(&[]);
    client
        .call("open_session", json!({ "input": input, "output": output }))
        .expect("resumed");
    client
        .call(
            "author_usda",
            json!({ "text": r#"over "World" { over "asset" { over "ball" ( prepend apiSchemas = ["CollectionAPI:lights"] ) { } } }"# }),
        )
        .expect("a collection");
    client
        .call(
            "bind_material",
            json!({ "prim": "/World/asset/ball", "material": "/World/Looks/red" }),
        )
        .expect("bound");
    client.finish();
    let stage = openusd::usd::Stage::open(output.to_str().unwrap()).expect("opens");
    let schemas: Vec<String> = stage
        .prim("/World/asset/ball")
        .unwrap()
        .authored_api_schemas()
        .unwrap()
        .iter()
        .map(|t| t.as_str().to_owned())
        .collect();
    assert!(
        schemas.contains(&"CollectionAPI:lights".to_owned())
            && schemas.contains(&"MaterialBindingAPI".to_owned()),
        "{schemas:?}"
    );
}

#[test]
fn a_failed_open_session_keeps_the_session_that_was_open() {
    let (mut client, _input, output) = cornellbox_session("bad_open", &[]);
    client
        .call(
            "set_attribute",
            json!({ "path": "/scene/Sky.inputs:exposure", "value": -1, "type": "float" }),
        )
        .expect("set");
    let e = client
        .call(
            "open_session",
            json!({ "input": output.with_file_name("missing.usda"), "output": output.with_file_name("other.usda") }),
        )
        .expect_err("no such input");
    assert!(e.contains("not a file"), "{e}");
    // The first session is still open, its history with it.
    let undone = client.call("undo", json!({})).expect("still open");
    assert_eq!(undone["undo_depth"], 0, "{undone:#}");
    client.finish();
}
