use super::Endpoint;
use serde_json::{json, Value};

fn reference(name: &str) -> Value {
    json!({"$ref":format!("#/components/schemas/{name}")})
}
fn example() -> Value {
    json!({"default":{"mode":"fixed","model":"primary/example-model","candidates":[]},"purposes":{},"instructions":"例行任务优先合适的经济模型。"})
}

pub(super) fn describe(endpoint: Endpoint) -> Endpoint {
    match (endpoint.method,endpoint.path) {
        ("put","/api/singles/:id/model-routing"|"/api/groups/:id/model-routing"|"/api/groups/:gid/agents/:identifier/model-routing")=>endpoint
            .json_request("ModelRouting",example()).json_response("ModelRoutingSaved",json!({"ok":true,"modelRouting":example()})),
        ("get","/api/singles/:id/model-routing"|"/api/groups/:gid/agents/:identifier/model-routing")=>endpoint
            .json_response("ModelRoutingPreview",json!({"modelRouting":null,"policies":[]})),
        ("get","/api/llm/catalog")=>endpoint.json_response("ModelCatalog",json!({"purposes":["chat","planning","coding","review","summary","research","vision","speech","transcribe","embedding","imageGen"],"models":[]})),
        _=>endpoint,
    }
}

pub(super) fn schemas() -> Value {
    let purpose = json!({"type":"string","enum":["chat","planning","coding","review","summary","research","vision","speech","transcribe","embedding","imageGen"]});
    let cost = json!({"type":"string","enum":["unknown","economy","balanced","premium"],"default":"unknown"});
    let text = json!({"allOf":[reference("ModelAssignment"),{"properties":{"mode":{"enum":["inherit","fixed","auto"]}}}]});
    let mut purposes = serde_json::Map::new();
    for name in [
        "chat", "planning", "coding", "review", "summary", "research",
    ] {
        purposes.insert(name.into(), text.clone());
    }
    for name in ["vision", "speech", "transcribe", "imageGen"] {
        purposes.insert(name.into(), reference("ModelAssignment"));
    }
    purposes.insert("embedding".into(),json!({"allOf":[reference("ModelAssignment"),{"properties":{"mode":{"enum":["inherit","fixed","disabled"]}}}]}));
    json!({
        "ModelPurpose":purpose,"ModelCost":cost,
        "ModelAssignment":{"type":"object","additionalProperties":false,"properties":{
            "mode":{"type":"string","enum":["inherit","fixed","auto","disabled"],"default":"inherit"},
            "model":{"type":"string","maxLength":256,"default":""},
            "candidates":{"type":"array","maxItems":64,"items":{"type":"string","minLength":1,"maxLength":256},"default":[]}
        },"allOf":[
            {"if":{"required":["mode"],"properties":{"mode":{"const":"fixed"}}},"then":{"required":["model"],"properties":{"model":{"minLength":1}}},"else":{"properties":{"model":{"maxLength":0}}}},
            {"if":{"required":["mode"],"properties":{"mode":{"const":"auto"}}},"then":{},"else":{"properties":{"candidates":{"maxItems":0}}}}
        ]},
        "ModelRouting":{"type":"object","additionalProperties":false,"description":"完整替换当前对象的模型规则；缺省字段继承。已有配置在首次保存前继续按旧字段解析。自动候选为空使用目录中 autoEligible 的模型，候选需适合当前用途。嵌入模型只能固定、继承或禁用。",
            "properties":{"default":text,"purposes":{"type":"object","additionalProperties":false,"properties":purposes},"instructions":{"type":"string","maxLength":4000,"default":""}},"examples":[example()]},
        "ModelRoutingSaved":{"type":"object","required":["ok","modelRouting"],"properties":{"ok":{"const":true},"modelRouting":reference("ModelRouting")}},
        "RoutingCandidate":{"type":"object","required":["reference","provider","model","cost","purposes","usageNotes"],"properties":{
            "reference":{"type":"string"},"provider":{"type":"string"},"model":{"type":"string"},"cost":reference("ModelCost"),"purposes":{"type":"array","items":reference("ModelPurpose")},"usageNotes":{"type":"string"}
        }},
        "RoutingPolicy":{"type":"object","required":["agentId","purpose","assignment","source","instructions","candidates"],"properties":{
            "agentId":{"type":"string"},"purpose":reference("ModelPurpose"),"assignment":reference("ModelAssignment"),"source":{"type":"string"},"instructions":{"type":"string"},"candidates":{"type":"array","items":reference("RoutingCandidate")}
        }},
        "ModelRoutingPreview":{"type":"object","required":["modelRouting","policies"],"properties":{
            "modelRouting":{"anyOf":[reference("ModelRouting"),{"type":"null"}]},"policies":{"type":"array","items":reference("RoutingPolicy")}
        }},
        "ModelCatalog":{"type":"object","required":["purposes","models"],"properties":{
            "purposes":{"type":"array","items":reference("ModelPurpose")},"models":{"type":"array","items":{"allOf":[reference("RoutingCandidate"),{"type":"object","required":["autoEligible","vision","audio","contextWindow"],"properties":{"autoEligible":{"type":"boolean"},"vision":{"type":"boolean"},"audio":{"type":"boolean"},"contextWindow":{"type":"integer","minimum":0}}}]}}
        }}
    })
}
