use crate::{EvalItem, Gold};
use dexter_core::{
    Action, AppSelector, Element, ElementId, ElementSource, Observation, Rect, SemanticTarget,
    Target, Window,
};
use dexter_decision::{
    CandidateGenerator, GenHistory, HeuristicGenerator, LastAct, Route, WorldDelta,
};

fn element(
    id: u64,
    parent: Option<u64>,
    depth: u32,
    role: &str,
    name: &str,
    value: Option<&str>,
    actions: &[&str],
) -> Element {
    Element {
        id: ElementId(id),
        parent: parent.map(ElementId),
        depth,
        role: Some(role.into()),
        name: Some(name.into()),
        value: value.map(str::to_string),
        enabled: Some(true),
        actions: actions.iter().map(|action| (*action).into()).collect(),
        source: ElementSource::Dom,
        ..Default::default()
    }
}

fn observation(title: &str, elements: Vec<Element>) -> Observation {
    let mut observation = Observation {
        app: Some(AppSelector::Name("Editor".into())),
        pid: Some(1),
        windows: vec![Window {
            id: 1,
            pid: 1,
            app: "Editor".into(),
            bundle_id: None,
            title: Some(title.into()),
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                w: 800.0,
                h: 600.0,
            },
            on_screen: true,
            layer: 0,
        }],
        elements,
        ..Default::default()
    };
    observation.digest = dexter_world_model::digest(&observation, 250);
    observation
}

fn modal_world() -> Observation {
    observation(
        "Editor de texto",
        vec![
            element(1, None, 0, "window", "Editor de texto", None, &[]),
            element(2, Some(1), 1, "button", "Guardar", None, &["press"]),
            element(
                3,
                Some(1),
                1,
                "dialog",
                "Actualización disponible",
                None,
                &[],
            ),
            element(4, Some(3), 2, "button", "Más tarde", None, &["press"]),
            element(5, Some(3), 2, "button", "Instalar ahora", None, &["press"]),
        ],
    )
}

fn twin_world() -> Observation {
    observation(
        "Tabla",
        vec![
            element(1, None, 0, "window", "Tabla", None, &[]),
            element(2, Some(1), 1, "row", "Juan", None, &[]),
            element(3, Some(2), 2, "static_text", "Juan", None, &[]),
            element(4, Some(2), 2, "button", "Eliminar", None, &["press"]),
            element(5, Some(1), 1, "row", "Marta", None, &[]),
            element(6, Some(5), 2, "static_text", "Marta", None, &[]),
            element(7, Some(5), 2, "button", "Eliminar", None, &["press"]),
        ],
    )
}

fn submit_world() -> Observation {
    observation(
        "Editor de texto",
        vec![
            element(1, None, 0, "window", "Editor de texto", None, &[]),
            element(
                2,
                Some(1),
                1,
                "text_field",
                "Contenido",
                Some("Hola"),
                &["set_value", "focus"],
            ),
            element(3, Some(1), 1, "button", "Enviar", None, &["press"]),
        ],
    )
}

fn complete_world() -> Observation {
    observation(
        "Editor de texto",
        vec![
            element(1, None, 0, "window", "Editor de texto", None, &[]),
            element(2, Some(1), 1, "static_text", "Mensaje enviado", None, &[]),
            element(
                3,
                Some(1),
                1,
                "button",
                "Enviar otro mensaje",
                None,
                &["press"],
            ),
        ],
    )
}

fn step_one_world() -> Observation {
    observation(
        "Editor de texto",
        vec![
            element(1, None, 0, "window", "Editor de texto", None, &[]),
            element(2, Some(1), 1, "button", "Siguiente", None, &["press"]),
        ],
    )
}

fn candidate_action(obs: &Observation, goal: &str, role: &str, name: &str) -> Action {
    HeuristicGenerator::default()
        .generate(obs, goal, &GenHistory::default())
        .into_iter()
        .find_map(|candidate| match &candidate.action {
            Action::Click {
                target: Target::Semantic(target),
                ..
            } if target.role.as_deref() == Some(role) && target.name.as_deref() == Some(name) => {
                Some(candidate.action)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no generated candidate for {role} {name:?} under {goal:?}"))
}

fn act_gold(element: u64, role: &str, name: &str) -> Gold {
    Gold::Act {
        target: SemanticTarget {
            role: Some(role.into()),
            name: Some(name.into()),
            ..Default::default()
        },
        element: ElementId(element),
    }
}

fn route_gold(route: Route) -> Gold {
    Gold::Route { route }
}

fn labeled(ids: &[&str]) -> Option<Vec<String>> {
    Some(ids.iter().map(|id| (*id).into()).collect())
}

fn item(
    id: &str,
    goal: &str,
    observation: Observation,
    gold: Gold,
    last_act: Option<LastAct>,
    expect_questions: Option<Vec<String>>,
    note: &str,
) -> EvalItem {
    EvalItem {
        id: id.into(),
        goal: goal.into(),
        observation,
        gold,
        last_act,
        expect_questions,
        source: "manual".into(),
        meta: serde_json::json!({"note": note}),
    }
}

pub fn generate_questions() -> Vec<EvalItem> {
    let modal = modal_world();
    let send_world = submit_world();
    let send_action = candidate_action(&send_world, "enviar el mensaje", "button", "Enviar");
    let next_action = candidate_action(
        &step_one_world(),
        "continuar al siguiente paso",
        "button",
        "Siguiente",
    );

    vec![
        item(
            "q-modal-unrelated",
            "guardar el documento",
            modal.clone(),
            route_gold(Route::EscalateHuman),
            None,
            labeled(&["blocking_modal"]),
            "Unrelated update dialog covers the requested save action.",
        ),
        item(
            "q-modal-relevant",
            "instalar la actualización ahora",
            modal,
            act_gold(5, "button", "Instalar ahora"),
            None,
            labeled(&[]),
            "The goal explicitly names the action inside the dialog.",
        ),
        item(
            "q-twin-pick",
            "eliminar la fila de Marta",
            twin_world(),
            act_gold(7, "button", "Eliminar"),
            None,
            labeled(&["disambiguate"]),
            "Row context identifies the second identical delete button.",
        ),
        item(
            "q-twin-none",
            "eliminar la fila de Pedro",
            twin_world(),
            route_gold(Route::Abstain),
            None,
            labeled(&["disambiguate"]),
            "Neither identical delete button matches the requested row.",
        ),
        item(
            "q-twin-distinct",
            "eliminar la fila de Marta",
            observation(
                "Tabla",
                vec![
                    element(1, None, 0, "window", "Tabla", None, &[]),
                    element(2, Some(1), 1, "button", "Eliminar Juan", None, &["press"]),
                    element(3, Some(1), 1, "button", "Eliminar Marta", None, &["press"]),
                ],
            ),
            act_gold(3, "button", "Eliminar Marta"),
            None,
            labeled(&[]),
            "Distinct button names already identify Marta without a question.",
        ),
        item(
            "q-effect-unchanged",
            "enviar el mensaje",
            send_world.clone(),
            route_gold(Route::EscalateHuman),
            Some(LastAct {
                action: send_action.clone(),
                delta: WorldDelta::Unchanged,
            }),
            labeled(&["action_effect"]),
            "A successful submit produced no observable world change.",
        ),
        item(
            "q-effect-unknown",
            "enviar el mensaje",
            Observation {
                elements_truncated: true,
                ..send_world.clone()
            },
            route_gold(Route::EscalateHuman),
            Some(LastAct {
                action: send_action.clone(),
                delta: WorldDelta::Unknown,
            }),
            labeled(&["action_effect"]),
            "Truncated perception cannot confirm whether submit changed the world.",
        ),
        item(
            "q-complete-yes",
            "enviar el mensaje",
            complete_world(),
            route_gold(Route::EscalateHuman),
            Some(LastAct {
                action: send_action,
                delta: WorldDelta::Changed,
            }),
            labeled(&["goal_complete"]),
            "Receipt text appeared after submit although structural completion is unverified.",
        ),
        item(
            "q-complete-no",
            "aceptar los términos y continuar",
            observation(
                "Editor de texto",
                vec![
                    element(1, None, 0, "window", "Editor de texto", None, &[]),
                    element(2, Some(1), 1, "static_text", "Paso 2 de 3", None, &[]),
                    element(3, Some(1), 1, "button", "Aceptar términos", None, &["press"]),
                    element(4, Some(1), 1, "button", "Atrás", None, &["press"]),
                ],
            ),
            act_gold(3, "button", "Aceptar términos"),
            Some(LastAct {
                action: next_action,
                delta: WorldDelta::Changed,
            }),
            labeled(&["goal_complete"]),
            "Step-1 goal: \"continuar al siguiente paso\". The changed world shows another step remains.",
        ),
        item(
            "q-plain",
            "guardar el documento",
            observation(
                "Editor de texto",
                vec![
                    element(1, None, 0, "window", "Editor de texto", None, &[]),
                    element(2, Some(1), 1, "button", "Guardar", None, &["press"]),
                ],
            ),
            act_gold(2, "button", "Guardar"),
            None,
            labeled(&[]),
            "The direct save action requires no typed judgment.",
        ),
    ]
}
