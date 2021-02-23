use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use rtic_syntax::ast::App;

use crate::{analyze::Analysis, check::Extra, codegen::util};

/// Generates code that runs before `#[init]`
pub fn codegen(app: &App, analysis: &Analysis, extra: &Extra) -> Vec<TokenStream2> {
    let mut stmts = vec![];

    let rt_err = util::rt_err_ident();

    // Disable interrupts -- `init` must run with interrupts disabled
    stmts.push(quote!(rtic::export::interrupt::disable();));

    // Populate the FreeQueue
    for (name, task) in &app.software_tasks {
        let cap = task.args.capacity;
        let fq_ident = util::fq_ident(name);
        let fq_ident = util::mark_internal_ident(&fq_ident);

        stmts.push(quote!(
            (0..#cap).for_each(|i| #fq_ident.enqueue_unchecked(i));
        ));
    }

    stmts.push(quote!(
        // To set the variable in cortex_m so the peripherals cannot be taken multiple times
        let mut core: rtic::export::Peripherals = rtic::export::Peripherals::steal().into();
    ));

    let device = &extra.device;
    let nvic_prio_bits = quote!(#device::NVIC_PRIO_BITS);

    let empty = "".to_string();
    let interrupt_ids = analysis
        .interrupts
        .iter()
        .map(|(p, (id, _))| (p, id, &empty));

    // Unmask interrupts and set their priorities
    let interrupts_g = interrupt_ids.chain(app.hardware_tasks.values().flat_map(|task| {
        task.args
            .binds
            .iter()
            .filter(|(bind, _cfg_name)| !util::is_exception(&bind))
            .map(move |(bind, cfg_name)| (&task.args.priority, bind, cfg_name))
    }));

    for (&priority, name, cfg) in interrupts_g {
        // Compile time assert that this priority is supported by the device
        stmts.push(quote!(let _ = [(); ((1 << #nvic_prio_bits) - #priority as usize)];));

        // NOTE this also checks that the interrupt exists in the `Interrupt` enumeration
        let interrupt = util::interrupt_ident();
        if cfg.is_empty() {
            stmts.push(quote!(
                core.NVIC.set_priority(
                    you_must_enable_the_rt_feature_for_the_pac_in_your_cargo_toml::#interrupt::#name,
                    rtic::export::logical2hw(#priority, #nvic_prio_bits),
                );
                rtic::export::NVIC::unmask(you_must_enable_the_rt_feature_for_the_pac_in_your_cargo_toml::#interrupt::#name);
            ));
        } else {
            stmts.push(quote!(
                if cfg!(configuration = #cfg) {
                    core.NVIC.set_priority(
                        you_must_enable_the_rt_feature_for_the_pac_in_your_cargo_toml::#interrupt::#name,
                        rtic::export::logical2hw(#priority, #nvic_prio_bits),
                    );

                    rtic::export::NVIC::unmask(you_must_enable_the_rt_feature_for_the_pac_in_your_cargo_toml::#interrupt::#name);
                }
            ));
        }

        // NOTE unmask the interrupt *after* setting its priority: changing the priority of a pended
        // interrupt is implementation defined
        stmts.push(quote!(rtic::export::NVIC::unmask(#rt_err::#interrupt::#name);));
    }

    // Set exception priorities
    let exceptions = app.hardware_tasks.values().flat_map(|task| {
        task.args
            .binds
            .iter()
            .filter(|(bind, _cfg_name)| util::is_exception(&bind))
            .map(move |(bind, cfg_name)| (&task.args.priority, bind, cfg_name))
    });
    for (priority, name, cfg) in exceptions {
        // // Compile time assert that this priority is supported by the device
        stmts.push(quote!(let _ = [(); ((1 << #nvic_prio_bits) - #priority as usize)];));

        if cfg.is_empty() {
            stmts.push(quote!(core.SCB.set_priority(
                rtic::export::SystemHandler::#name,
                rtic::export::logical2hw(#priority, #nvic_prio_bits),
            );));
        } else {
            stmts.push(quote!(
            if cfg!(configuration = #cfg) {
                core.SCB.set_priority(
                    rtic::export::SystemHandler::#name,
                    rtic::export::logical2hw(#priority, #nvic_prio_bits),
                );
            }));
        }
    }

    // Initialize monotonic's interrupts
    for (_, monotonic) in app.monotonics.iter()
    //.map(|(ident, monotonic)| (ident, &monotonic.args.priority, &monotonic.args.binds))
    {
        let priority = &monotonic.args.priority;
        let binds = &monotonic.args.binds;

        // Compile time assert that this priority is supported by the device
        stmts.push(quote!(let _ = [(); ((1 << #nvic_prio_bits) - #priority as usize)];));

        let app_name = &app.name;
        let app_path = quote! {crate::#app_name};
        let mono_type = &monotonic.ty;

        if &*binds.to_string() == "SysTick" {
            stmts.push(quote!(
                core.SCB.set_priority(
                    rtic::export::SystemHandler::SysTick,
                    rtic::export::logical2hw(#priority, #nvic_prio_bits),
                );

                // Always enable monotonic interrupts if they should never be off
                if !<#mono_type as rtic::Monotonic>::DISABLE_INTERRUPT_ON_EMPTY_QUEUE {
                    core::mem::transmute::<_, cortex_m::peripheral::SYST>(())
                        .enable_interrupt();
                }
            ));
        } else {
            // NOTE this also checks that the interrupt exists in the `Interrupt` enumeration
            let interrupt = util::interrupt_ident();
            stmts.push(quote!(
                core.NVIC.set_priority(
                    #rt_err::#interrupt::#binds,
                    rtic::export::logical2hw(#priority, #nvic_prio_bits),
                );

                // Always enable monotonic interrupts if they should never be off
                if !<#mono_type as rtic::Monotonic>::DISABLE_INTERRUPT_ON_EMPTY_QUEUE {
                    rtic::export::NVIC::unmask(#app_path::#rt_err::#interrupt::#binds);
                }
            ));
        }
    }

    // If there's no user `#[idle]` then optimize returning from interrupt handlers
    if app.idles.is_empty() {
        // Set SLEEPONEXIT bit to enter sleep mode when returning from ISR
        stmts.push(quote!(core.SCB.scr.modify(|r| r | 1 << 1);));
    }

    stmts
}
