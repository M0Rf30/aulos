// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! A key-value store in IndexedDB: the one browser storage that keeps big
//! binary values (the library snapshot) and structured-cloneable objects
//! (the folder handles, which can live nowhere else).

use std::cell::RefCell;

use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Event, IdbDatabase, IdbRequest, IdbTransaction, IdbTransactionMode};

const DB_NAME: &str = "aulos";
const STORE: &str = "kv";

thread_local! {
    static DB: RefCell<Option<IdbDatabase>> = const { RefCell::new(None) };
}

/// A readable message for a JS error value.
pub(super) fn describe(error: &JsValue) -> String {
    if let Some(message) = error
        .dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| {
            error
                .dyn_ref::<web_sys::DomException>()
                .map(|e| e.message())
        })
        .or_else(|| error.as_string())
    {
        message
    } else {
        format!("{error:?}")
    }
}

/// The `name` of a JS error (`NotAllowedError`, `NotFoundError`, …).
pub(super) fn error_name(error: &JsValue) -> String {
    js_sys::Reflect::get(error, &JsValue::from_str("name"))
        .ok()
        .and_then(|name| name.as_string())
        .unwrap_or_default()
}

/// Resolves with the request's result, rejects with its error.
fn request_promise(request: &IdbRequest) -> Promise {
    Promise::new(&mut |resolve, reject| {
        let ok_request = request.clone();
        let on_success = Closure::once_into_js(move |_: Event| {
            let result = ok_request.result().unwrap_or(JsValue::UNDEFINED);
            let _ = resolve.call1(&JsValue::NULL, &result);
        });
        let err_request = request.clone();
        let on_error = Closure::once_into_js(move |_: Event| {
            let message = err_request
                .error()
                .ok()
                .flatten()
                .map_or_else(|| "IndexedDB request failed".to_owned(), |e| e.message());
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&message));
        });
        request.set_onsuccess(Some(on_success.unchecked_ref()));
        request.set_onerror(Some(on_error.unchecked_ref()));
    })
}

/// The event handlers of a transaction, kept alive until the transaction
/// settled (dropped by the caller after awaiting its promise).
type TransactionHandlers = Vec<Closure<dyn FnMut(Event)>>;

/// Resolves when the transaction committed, rejects when it failed or
/// aborted. A failed request fires `error` and then `abort`; the handlers are
/// reusable closures and settling a promise twice is a no-op, so the second
/// event is harmless.
fn transaction_promise(tx: &IdbTransaction) -> (Promise, TransactionHandlers) {
    let mut handlers = TransactionHandlers::new();
    let promise = Promise::new(&mut |resolve, reject| {
        let on_complete = Closure::<dyn FnMut(Event)>::new(move |_| {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let failed_tx = tx.clone();
        let failed = Closure::<dyn FnMut(Event)>::new(move |_| {
            let message = failed_tx
                .error()
                .map_or_else(|| "IndexedDB write failed".to_owned(), |e| e.message());
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&message));
        });
        tx.set_oncomplete(Some(on_complete.as_ref().unchecked_ref()));
        tx.set_onerror(Some(failed.as_ref().unchecked_ref()));
        tx.set_onabort(Some(failed.as_ref().unchecked_ref()));
        handlers.push(on_complete);
        handlers.push(failed);
    });
    (promise, handlers)
}

/// Forgets the cached connection when the browser closes it or another tab
/// needs to upgrade the database, so the next call opens a fresh one.
fn watch_connection(db: &IdbDatabase) {
    let dropped = {
        let db = db.clone();
        Closure::<dyn FnMut(Event)>::new(move |_| {
            db.close();
            DB.with(|slot| *slot.borrow_mut() = None);
        })
        .into_js_value()
    };
    db.set_onversionchange(Some(dropped.unchecked_ref()));
    db.set_onclose(Some(dropped.unchecked_ref()));
}

async fn open() -> Result<IdbDatabase, String> {
    if let Some(db) = DB.with(|db| db.borrow().clone()) {
        return Ok(db);
    }
    let factory = web_sys::window()
        .ok_or("no window")?
        .indexed_db()
        .map_err(|e| describe(&e))?
        .ok_or("IndexedDB is not available")?;
    let request = factory
        .open_with_u32(DB_NAME, 1)
        .map_err(|e| describe(&e))?;
    let on_upgrade = {
        let request = request.clone();
        Closure::once_into_js(move |_: Event| {
            if let Ok(db) = request.result().and_then(|r| r.dyn_into::<IdbDatabase>()) {
                let _ = db.create_object_store(STORE);
            }
        })
    };
    request.set_onupgradeneeded(Some(on_upgrade.unchecked_ref()));
    let db: IdbDatabase = JsFuture::from(request_promise(&request))
        .await
        .map_err(|e| describe(&e))?
        .dyn_into()
        .map_err(|_| "IndexedDB returned no database".to_owned())?;
    // Another call may have opened the database while this one waited: keep
    // that connection and close this one, so there is only ever one.
    Ok(DB.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(existing) = slot.as_ref() {
            db.close();
            return existing.clone();
        }
        watch_connection(&db);
        *slot = Some(db.clone());
        db
    }))
}

fn store_tx(db: &IdbDatabase, mode: IdbTransactionMode) -> Result<IdbTransaction, String> {
    db.transaction_with_str_and_mode(STORE, mode)
        .map_err(|e| describe(&e))
}

/// The value stored under `key`.
///
/// # Errors
/// A message when IndexedDB is unavailable or the read failed.
pub(super) async fn get(key: &str) -> Result<Option<JsValue>, String> {
    let db = open().await?;
    let tx = store_tx(&db, IdbTransactionMode::Readonly)?;
    let store = tx.object_store(STORE).map_err(|e| describe(&e))?;
    let request = store
        .get(&JsValue::from_str(key))
        .map_err(|e| describe(&e))?;
    let value = JsFuture::from(request_promise(&request))
        .await
        .map_err(|e| describe(&e))?;
    Ok((!value.is_undefined() && !value.is_null()).then_some(value))
}

/// Stores `value` under `key`, resolving once the write is committed.
///
/// # Errors
/// A message when IndexedDB is unavailable, full, or refused the value.
pub(super) async fn put(key: &str, value: &JsValue) -> Result<(), String> {
    let db = open().await?;
    let tx = store_tx(&db, IdbTransactionMode::Readwrite)?;
    let (done, _handlers) = transaction_promise(&tx);
    if let Err(e) = tx
        .object_store(STORE)
        .and_then(|store| store.put_with_key(value, &JsValue::from_str(key)))
    {
        // The handlers drop with this frame; the transaction may still fire.
        tx.set_oncomplete(None);
        tx.set_onerror(None);
        tx.set_onabort(None);
        return Err(describe(&e));
    }
    JsFuture::from(done)
        .await
        .map(|_| ())
        .map_err(|e| describe(&e))
}
