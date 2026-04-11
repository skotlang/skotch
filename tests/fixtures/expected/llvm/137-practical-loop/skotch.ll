; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_isPrime(i32 %arg0) {
entry:
  %merge_7 = alloca i32
  %t0 = add i32 0, 2
  %t1 = icmp slt i32 %arg0, %t0
  br i1 %t1, label %bb1, label %bb2
bb1:
  %t2 = add i32 0, 0
  ret i32 %t2
bb2:
  br label %bb3
bb3:
  %t3 = add i32 0, 2
  store i32 %t3, ptr %merge_7
  br label %bb4
bb4:
  %t4 = load i32, ptr %merge_7
  %t5 = load i32, ptr %merge_7
  %t6 = mul i32 %t4, %t5
  %t7 = icmp sle i32 %t6, %arg0
  br i1 %t7, label %bb5, label %bb6
bb5:
  %t8 = load i32, ptr %merge_7
  %t9 = srem i32 %arg0, %t8
  %t10 = add i32 0, 0
  %t11 = icmp eq i32 %t9, %t10
  br i1 %t11, label %bb7, label %bb8
bb6:
  %t12 = add i32 0, 1
  ret i32 %t12
bb7:
  %t13 = add i32 0, 0
  ret i32 %t13
bb8:
  br label %bb9
bb9:
  %t14 = load i32, ptr %merge_7
  %t15 = add i32 0, 1
  %t16 = add i32 %t14, %t15
  store i32 %t16, ptr %merge_7
  br label %bb4
}

define i32 @main() {
entry:
  %merge_2 = alloca i32
  %t0 = add i32 0, 2
  %t1 = add i32 0, 20
  store i32 %t0, ptr %merge_2
  br label %bb1
bb1:
  %t2 = load i32, ptr %merge_2
  %t3 = icmp sle i32 %t2, %t1
  br i1 %t3, label %bb2, label %bb3
bb2:
  %t4 = load i32, ptr %merge_2
  %t5 = call i32 @InputKt_isPrime(i32 %t4)
  %t6 = trunc i32 %t5 to i1
  br i1 %t6, label %bb4, label %bb5
bb3:
  ret i32 0
bb4:
  %t7 = load i32, ptr %merge_2
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t7)
  br label %bb6
bb5:
  br label %bb6
bb6:
  %t9 = add i32 0, 1
  %t10 = load i32, ptr %merge_2
  %t11 = add i32 %t10, %t9
  store i32 %t11, ptr %merge_2
  br label %bb1
}

