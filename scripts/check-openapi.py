#!/usr/bin/env python3
"""Validate an exported OpenAPI 3.1 document, its examples and optional REST fixtures."""
import argparse
import json
from pathlib import Path

from jsonschema import Draft202012Validator
from openapi_spec_validator import OpenAPIV31SpecValidator


def validate_schema(document, schema, value):
    # Local references in operation schemas resolve against the exported components.
    root = {**schema, "components": document["components"]}
    Draft202012Validator(root).validate(value)


def check(document, exchanges):
    OpenAPIV31SpecValidator(document).validate()
    for schema in document["components"].get("schemas", {}).values():
        Draft202012Validator.check_schema(schema)
    examples = 0
    operations = 0
    for path, item in document["paths"].items():
        for method, operation in item.items():
            if method not in {"get", "put", "post", "delete", "patch", "head", "options", "trace"}:
                continue
            operations += 1
            bodies = [operation.get("requestBody", {})]
            bodies.extend(operation["responses"].values())
            for body in bodies:
                for media in body.get("content", {}).values():
                    if "example" in media:
                        try:
                            validate_schema(document, media["schema"], media["example"])
                        except Exception as error:
                            raise ValueError(f"Invalid example: {method} {path}") from error
                        examples += 1
    for exchange in exchanges:
        operation = document["paths"][exchange["path"]][exchange["method"]]
        request = operation.get("requestBody", {}).get("content", {}).get("application/json")
        if request is not None and exchange["status"] < 400:
            validate_schema(document, request["schema"], exchange["request"])
        response = operation["responses"][str(exchange["status"])].get("content", {}).get("application/json")
        if response is not None:
            validate_schema(document, response["schema"], exchange["response"])
    for schema in document["components"].get("schemas", {}).values():
        for example in schema.get("examples", []):
            validate_schema(document, schema, example)
            examples += 1
    return {"paths": len(document["paths"]), "operations": operations, "examples": examples, "exchanges": len(exchanges)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("document", type=Path, help="JSON produced by exm openapi --out")
    parser.add_argument("--exchanges", type=Path, help="Optional isolated REST exchanges produced by openapi_contract tests")
    args = parser.parse_args()
    document = json.loads(args.document.read_text())
    exchanges = json.loads(args.exchanges.read_text()) if args.exchanges else []
    if args.exchanges and not exchanges:
        raise ValueError("The supplied REST exchange fixture is empty")
    print(json.dumps(check(document, exchanges), ensure_ascii=False))


if __name__ == "__main__":
    main()
