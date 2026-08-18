use actix_web::{get, web, HttpRequest, HttpResponse};

const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/zy97/qr_server/master/scripts/install-print-agent.ps1";

fn websocket_url(request: &HttpRequest) -> String {
    let connection = request.connection_info();
    let scheme = if connection.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    format!("{}://{}/ws/agent", scheme, connection.host())
}

fn installer_script(server_url: &str) -> String {
    format!(
        "$script = Invoke-RestMethod '{}'; & (:Create($script)) -ServerUrl '{}'\r\n",
        INSTALL_SCRIPT_URL,
        server_url.replace('\'', "''")
    )
}

#[get("/install-print-agent.ps1")]
async fn install_print_agent(request: HttpRequest) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/plain; charset=utf-8")
        .append_header((
            "Content-Disposition",
            "inline; filename=install-print-agent.ps1",
        ))
        .body(installer_script(&websocket_url(&request)))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(install_print_agent);
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test;

    #[actix_web::test]
    async fn installer_uses_request_host_and_ws_scheme() {
        let request = test::TestRequest::default()
            .insert_header(("Host", "192.168.1.10:9095"))
            .to_http_request();

        assert_eq!(websocket_url(&request), "ws://192.168.1.10:9095/ws/agent");
        assert!(installer_script(&websocket_url(&request))
            .contains("-ServerUrl 'ws://192.168.1.10:9095/ws/agent'"));
    }

    #[actix_web::test]
    async fn installer_uses_wss_for_https_requests() {
        let request = test::TestRequest::default()
            .uri("https://print.example.com/install-print-agent.ps1")
            .insert_header(("Host", "print.example.com"))
            .to_http_request();

        assert_eq!(websocket_url(&request), "wss://print.example.com/ws/agent");
    }
}
