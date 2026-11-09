// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
// Exercise the real bridge with the committed audio fixture: pause, seek, rate, resume.
#include <QApplication>
#include <QJsonDocument>
#include <QJsonObject>
#include <QTimer>
#include <cstdint>
#include <cstdio>
extern "C" {
void day_media_set_state_cb(void (*)(uint64_t, int, const char *));
void *day_media_new(uint64_t, const char *, int, int, int, int, double);
void day_media_pause(void *);
void day_media_play(void *);
void day_media_seek(void *, double);
void day_media_rate(void *, double);
void day_media_poll(void *);
}
static int state = -1;
static double position = 0;
int main(int argc, char **argv) {
    QApplication app(argc, argv);
    if (argc != 2) return 2;
    day_media_set_state_cb([](uint64_t, int value, const char *message) {
        if (value == 7) {
            position = QJsonDocument::fromJson(message).object()["position"].toDouble();
        } else {
            state = value;
            std::printf("state=%d %s\n", value, message);
        }
    });
    auto *player = day_media_new(1, argv[1], 1, 0, 0, 1, 0.0);
    bool paused = false;
    int phase = 0;
    QTimer poll;
    QObject::connect(&poll, &QTimer::timeout, [&]() {
        day_media_poll(player);
        if (phase == 0 && state == 2 && position > 0.3) {
            day_media_pause(player);
            phase = 1;
        } else if (phase == 1 && state == 3) {
            paused = true;
            day_media_seek(player, 5.0);
            day_media_rate(player, 1.5);
            day_media_play(player);
            phase = 2;
        } else if (phase == 2 && state == 2 && position > 7.0) {
            std::printf("paused=%d final=%d position=%.3f\n", paused, state, position);
            app.exit(0);
        }
    });
    poll.start(100);
    // Follow observed states rather than assuming the decoder starts within a fixed delay
    // on a busy CI runner. BufferingMedia may persist while playback progresses: both the
    // public state and position must prove a successful resume before this deadline.
    QTimer::singleShot(30000, [&]() {
        std::printf("paused=%d final=%d position=%.3f\n", paused, state, position);
        app.exit(1);
    });
    return app.exec();
}
