//! Генерация типов сообщений из `proto/*.proto`.
//!
//! Схему разбирает `protox` (компилятор protobuf на чистом Rust) в описание
//! `FileDescriptorSet`, а `micropb-gen` строит из него код — так системный
//! `protoc` не нужен нигде. Код ложится в `OUT_DIR`, в git его нет: схема и
//! код не могут разойтись.

use std::fmt::Write as _;
use std::path::PathBuf;

use micropb_gen::service::{ServiceGenerator, ServiceView, TypeResolver};
use micropb_gen::{Config, Generator};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// Схемы, из которых генерируется код.
const PROTOS: &[&str] = &["proto/host.proto"];

/// Ёмкости полей переменной длины, байт: строки и `bytes` —
/// `heapless`-контейнеры, и длину надо знать заранее. Длиннее на проводе —
/// сообщение не разберётся. Имя константы крейта, поле схемы, ёмкость.
const LIMITS: &[(&str, &str, u32)] = &[
    ("VERSION_BYTES", ".host.Info.version", 32),
    ("COMMIT_BYTES", ".host.Info.commit", 16),
    ("CHIP_BYTES", ".host.Info.chip", 24),
    ("UID_BYTES", ".host.Info.uid", 12),
    // Самое длинное имя причины сброса (`bsp::reset::take_cause`) —
    // `independent_watchdog`, 20.
    ("RESET_BYTES", ".host.Info.reset", 24),
    // `domain::device::PANIC_BRIEF`.
    ("PANIC_BYTES", ".host.Info.last_panic", 96),
    ("ERROR_TEXT_BYTES", ".host.Error.text", 64),
    // {%- if ota == "true" %}
    // Те же пределы, что у команды узлу OTA (`domain::ota::command`:
    // `SIGNATURE_BYTES`, `CHUNK_BYTES`): длиннее не дойдёт до узла.
    ("OTA_SIGNATURE_BYTES", ".host.Ota.Start.signature", 64),
    ("OTA_CHUNK_BYTES", ".host.Ota.Chunk.data", 128),
    // {%- endif %}
];

/// Ёмкости повторяющихся полей (`repeated`), элементов: `heapless::Vec` с
/// ёмкостью в типе. Без строки здесь micropb-gen не соберёт схему с таким полем
/// (`Missing max_len`). Имя константы крейта, поле схемы, ёмкость.
const LENGTHS: &[(&str, &str, u32)] = &[
    // ("READINGS_LEN", ".host.Telemetry.readings", 8),
];

/// Трейт на protobuf-сервис: модуль подсистемы реализует его у себя, а общий
/// `Handlers` разводит команды по сервисам.
struct HostServices;

impl ServiceGenerator for HostServices {
    fn generate(&mut self, service: &ServiceView<'_>, resolver: &dyn TypeResolver) -> TokenStream {
        let prefix = service_prefix(service.name);
        let trait_name = format_ident!("Handle{prefix}");
        let service_name = service.name;
        let service_doc = format!("Обработчик команд protobuf-сервиса `{service_name}`.");

        let methods = service.methods.iter().map(|method| {
            let method_name = format_ident!("{}", snake_case(method.name));
            let input = resolver.rust_path(method.input_type);
            let doc = format!("Команда `{}`.", method.name);
            quote! {
                #[doc = #doc]
                async fn #method_name(&mut self, request: &#input) -> Response_::Result;
            }
        });

        quote! {
            #[doc = #service_doc]
            ///
            /// Модуль подсистемы реализует его у себя; общий `Handlers`
            /// разводит команды по сервисам.
            #[allow(
                async_fn_in_trait,
                reason = "трейт реализует модуль подсистемы; dyn и Send не нужны"
            )]
            pub trait #trait_name {
                #(#methods)*
            }
        }
    }
}

/// Имя сервиса без суффикса `Service` (`OtaService` → `Ota`).
fn service_prefix(name: &str) -> &str {
    name.strip_suffix("Service")
        .unwrap_or_else(|| panic!("service `{name}` must end with `Service`"))
}

/// `Info` → `info`; имена RPC — `PascalCase`.
fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.char_indices() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(std::env::var("OUT_DIR")?);
    for proto in PROTOS {
        println!("cargo:rerun-if-changed={proto}");
    }

    let mut compiler = protox::Compiler::new(["proto"])?;
    compiler.include_imports(true);
    compiler.open_files(PROTOS)?;
    let descriptor = compiler.encode_file_descriptor_set();
    let fdset = compiler.file_descriptor_set();
    let descriptor_path = out.join("host.fdset");
    std::fs::write(&descriptor_path, descriptor)?;

    let mut generator = Generator::new();
    generator.use_container_heapless();
    generator.add_service_generator(HostServices);
    for (_, field, bytes) in LIMITS {
        generator.configure(field, Config::new().max_bytes(*bytes));
    }
    for (_, field, len) in LENGTHS {
        generator.configure(field, Config::new().max_len(*len));
    }
    generator.compile_fdset_file(&descriptor_path, out.join("host.rs"))?;

    // Общий разбор команд: `Handlers` знает все сервисы схемы и их методы,
    // поэтому `domain::protobuf::Service` не пишет ни одного `if` на сервис.
    // Только целевые файлы: у сервисов из импортов `compile_fdset_file` трейтов
    // не создаёт.
    let dispatch = {
        let mut fields = Vec::new();
        let mut generics = Vec::new();
        let mut bounds = Vec::new();
        let mut arms = Vec::new();
        for file in fdset
            .file
            .iter()
            .filter(|file| PROTOS.iter().any(|proto| proto.ends_with(file.name())))
        {
            for service in &file.service {
                let prefix = service_prefix(service.name());
                let field = format_ident!("{}", snake_case(prefix));
                let generic = format_ident!("H{}", generics.len());
                let trait_name = format_ident!("Handle{prefix}");
                fields.push(quote! { pub #field: #generic });
                bounds.push(quote! { #generic: host_::#trait_name });
                for method in &service.method {
                    let method_name = format_ident!("{}", snake_case(method.name()));
                    let variant = format_ident!("{}", method.name());
                    arms.push(quote! {
                        Some(host_::Request_::Command::#variant(request)) => {
                            Some(self.#field.#method_name(request).await)
                        }
                    });
                }
                generics.push(generic);
            }
        }
        quote! {
            /// Обработчики сервисов схемы: разводит команды по подсистемам.
            #[derive(Debug)]
            pub struct Handlers<#(#generics),*> {
                #(#fields),*
            }
            impl<#(#generics),*> Handlers<#(#generics),*>
            where
                #(#bounds),*
            {
                /// Разбирает команду и отдаёт её обработчику своего сервиса.
                pub async fn handle(
                    &mut self,
                    request: &host_::Request,
                ) -> Option<host_::Response_::Result> {
                    match &request.command {
                        #(#arms)*
                        _ => None,
                    }
                }
            }
        }
    };
    std::fs::write(out.join("dispatch.rs"), dispatch.to_string())?;

    // Те же ёмкости — константами крейта: тот, кто кладёт в поле строку своей
    // длины (`domain::protobuf::Service` — сведения об устройстве), сверяет её при
    // сборке обработчика (`Service::new`: длинная строка — паника при старте, в
    // `const`-контексте — ошибка компиляции), а не узнаёт об обрезке на проводе.
    let mut limits = String::new();
    for (name, field, bytes) in LIMITS {
        writeln!(
            limits,
            "/// Ёмкость `{field}`, байт (`build.rs`).\npub const {name}: usize = {bytes};"
        )?;
    }
    for (name, field, len) in LENGTHS {
        writeln!(
            limits,
            "/// Ёмкость `{field}`, элементов (`build.rs`).\npub const {name}: usize = {len};"
        )?;
    }
    std::fs::write(out.join("limits.rs"), limits)?;
    Ok(())
}
