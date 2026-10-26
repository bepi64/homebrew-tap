BINARY := bin/git-tui
PACKAGE := .

export CGO_ENABLED := 1

.PHONY: build sign run test vet fmt tidy clean

build:
	go build -o $(BINARY) $(PACKAGE)

sign: build
	codesign -s - --force --options runtime $(BINARY)

run: sign
	./$(BINARY) $(ARGS)

test:
	go test ./...

vet:
	go vet ./...

fmt:
	gofmt -l .

tidy:
	go mod tidy

clean:
	rm -rf bin
