// API の定義（main.tsp）から、ツールの一覧 generated/tools.json を作る。
//
//   node build.mjs          作り直す
//   node build.mjs --check  作り直したものとコミットしてあるものが同じかを確かめる（違えば終了コード 1）
//
// アプリ（Rust）は tools.json を実行ファイルに埋め込んで読む。ビルドに Node は要らない。

import { readFile, writeFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import {
  compile,
  NodeHost,
  getDoc,
  getMinValue,
  getMaxValue,
  formatDiagnostic,
} from "@typespec/compiler";
import { has } from "./lib.js";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "generated", "tools.json");

/** ツールの組。アプリの中の AI・MCP・コマンドのどれに出すかが組で決まる。 */
const GROUPS = [
  // データベースの操作。アプリの中の AI、MCP、コマンドのすべてに出す
  ["Data", "data"],
  // 画面と設定の操作。アプリの中の AI にだけ出す
  ["App", "app"],
  // MCP のサーバーだけが持つ操作
  ["Mcp", "mcp"],
];

const program = await compile(NodeHost, join(here, "main.tsp"), { noEmit: true });
if (program.diagnostics.length > 0) {
  for (const d of program.diagnostics) console.error(formatDiagnostic(d));
  process.exit(1);
}

const failures = [];
const fail = (where, text) => failures.push(`${where}: ${text}`);

/** スカラーの元をたどって、JSON Schema の型を決める。 */
function scalarType(scalar, where) {
  for (let s = scalar; s; s = s.baseScalar) {
    switch (s.name) {
      case "string":
        return "string";
      case "boolean":
        return "boolean";
      case "integer":
      case "int8":
      case "int16":
      case "int32":
      case "int64":
      case "uint8":
      case "uint16":
      case "uint32":
      case "uint64":
      case "safeint":
        return "integer";
      case "numeric":
      case "float":
      case "float32":
      case "float64":
      case "decimal":
        return "number";
    }
  }
  fail(where, `型 ${scalar.name} は JSON Schema にできません`);
  return "string";
}

/** スカラーとその元に付けた最小値・最大値。 */
function scalarLimits(scalar) {
  const limits = {};
  for (let s = scalar; s; s = s.baseScalar) {
    const min = getMinValue(program, s);
    const max = getMaxValue(program, s);
    if (limits.minimum === undefined && min !== undefined) limits.minimum = min;
    if (limits.maximum === undefined && max !== undefined) limits.maximum = max;
  }
  return limits;
}

// 説明（/** */）は行を区切りなしでつなぐ。日本語の文を、文の終わりで折り返して書けるようにするため
const docOf = (type) => getDoc(program, type)?.replace(/\n/g, "");

const isNull = (t) => t.kind === "Intrinsic" && t.name === "null";

/** null を足す。`type` を配列にし、`enum` があれば null も入れる。 */
function withNull(schema) {
  const type = Array.isArray(schema.type) ? schema.type : [schema.type];
  const result = { ...schema, type: [...type, "null"] };
  if (schema.enum) result.enum = [...schema.enum, null];
  return result;
}

function schemaOf(type, where) {
  switch (type.kind) {
    case "Scalar":
      return { type: scalarType(type, where), ...scalarLimits(type) };
    case "Boolean":
      return { type: "boolean" };
    case "Union":
    case "Enum":
      return unionSchema(type, where);
    case "Model":
      if (type.indexer && type.name === "Array") {
        return { type: "array", items: schemaOf(type.indexer.value, `${where}[]`) };
      }
      if (type.indexer && type.name === "Record") {
        return {
          type: "object",
          additionalProperties: schemaOf(type.indexer.value, `${where}{}`),
        };
      }
      return objectSchema(type, where);
    default:
      fail(where, `${type.kind} は JSON Schema にできません`);
      return {};
  }
}

function unionSchema(type, where) {
  const variants =
    type.kind === "Enum"
      ? [...type.members.values()].map((m) => ({ kind: "String", value: m.value ?? m.name }))
      : [...type.variants.values()].map((v) => v.type);
  const nullable = variants.some(isNull);
  const rest = variants.filter((v) => !isNull(v));
  let schema;
  if (rest.length > 0 && rest.every((v) => v.kind === "String")) {
    schema = { type: "string", enum: rest.map((v) => v.value) };
  } else if (rest.length > 0 && rest.every((v) => v.kind === "Number")) {
    const values = rest.map((v) => v.value);
    schema = { type: values.every(Number.isInteger) ? "integer" : "number", enum: values };
  } else if (rest.length === 1) {
    schema = schemaOf(rest[0], where);
  } else {
    fail(where, "文字列か数の候補か、1 つの型と null の組み合わせだけを使えます");
    return {};
  }
  return nullable ? withNull(schema) : schema;
}

/** 項目の並び（`properties` と `required`）。 */
function propertiesOf(props, where) {
  const properties = {};
  const required = [];
  for (const [name, prop] of props) {
    const at = `${where}.${name}`;
    const schema = schemaOf(prop.type, at);
    const min = getMinValue(program, prop);
    const max = getMaxValue(program, prop);
    if (min !== undefined) schema.minimum = min;
    if (max !== undefined) schema.maximum = max;
    const doc = docOf(prop);
    if (doc) schema.description = doc;
    properties[name] = schema;
    if (!prop.optional) required.push(name);
  }
  return { properties, required };
}

function objectSchema(model, where) {
  const { properties, required } = propertiesOf(model.properties, where);
  const schema = { type: "object", properties };
  if (required.length > 0) schema.required = required;
  if (has(program, "closed", model)) schema.additionalProperties = false;
  return schema;
}

const tools = [];
const root = program.getGlobalNamespaceType().namespaces.get("Malakt");
if (!root) {
  console.error("main.tsp に namespace Malakt がありません");
  process.exit(1);
}
for (const [namespace, group] of GROUPS) {
  const ns = root.namespaces.get(namespace);
  if (!ns) {
    fail("Malakt", `namespace ${namespace} がありません`);
    continue;
  }
  for (const [name, op] of ns.operations) {
    const where = `${namespace}.${name}`;
    const description = docOf(op);
    if (!description) fail(where, "説明（/** */）がありません");
    // ツールの引数は、どれも書いていない項目を受け付けない
    const { properties, required } = propertiesOf(op.parameters.properties, where);
    const tool = {
      name,
      group,
      description: description ?? "",
      input_schema: { type: "object", properties, required, additionalProperties: false },
    };
    for (const mark of ["writes", "confirm", "inAppOnly", "view", "session", "localFiles"]) {
      if (has(program, mark, op)) tool[mark.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`)] = true;
    }
    tools.push(tool);
  }
}

// 一緒に渡す組の中で、名前が重ならないこと（アプリの AI は data と app、MCP は data と mcp）
for (const together of [
  ["data", "app"],
  ["data", "mcp"],
]) {
  const names = new Set();
  for (const tool of tools.filter((t) => together.includes(t.group))) {
    if (names.has(tool.name)) fail(tool.name, `${together.join(" と ")} に同じ名前のツールがあります`);
    names.add(tool.name);
  }
}

if (failures.length > 0) {
  for (const f of failures) console.error(f);
  process.exit(1);
}

const text = `${JSON.stringify({ tools }, null, 2)}\n`;
if (process.argv.includes("--check")) {
  const current = await readFile(out, "utf8").catch(() => "");
  if (current !== text) {
    console.error(
      "api/generated/tools.json が api/*.tsp と合っていません。`mise run api` で作り直してください。",
    );
    process.exit(1);
  }
} else {
  await mkdir(dirname(out), { recursive: true });
  await writeFile(out, text);
  console.log(`${tools.length} 個のツールを ${out} に書きました`);
}
