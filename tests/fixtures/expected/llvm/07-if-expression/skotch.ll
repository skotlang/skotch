; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %merge_1 = alloca i32
  %t0 = add i32 0, 1
  %t1 = trunc i32 %t0 to i1
  br i1 %t1, label %bb1, label %bb2
bb1:
  %t2 = add i32 0, 1
  store i32 %t2, ptr %merge_1
  br label %bb3
bb2:
  %t3 = add i32 0, 2
  store i32 %t3, ptr %merge_1
  br label %bb3
bb3:
  %t4 = load i32, ptr %merge_1
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t4)
  ret i32 0
}

